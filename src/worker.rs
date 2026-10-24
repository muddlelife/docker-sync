use crate::{
    domain::*,
    error::{Error, Result},
    github::{DispatchError, DispatchedRun, GithubApi},
    redact::{payload_secrets, redact},
    store::{Store, now},
};
use std::sync::Arc;
pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> i64 {
        now()
    }
}
#[derive(Clone)]
pub struct Worker {
    pub store: Store,
    pub api: Arc<dyn GithubApi>,
    clock: Arc<dyn Clock>,
    gate: Arc<rocket::tokio::sync::Mutex<()>>,
}
impl Worker {
    pub fn new(store: Store, api: Arc<dyn GithubApi>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            api,
            clock,
            gate: Arc::new(rocket::tokio::sync::Mutex::new(())),
        }
    }
    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        rocket::tokio::task::spawn_blocking(move || f(store))
            .await
            .map_err(|_| Error::Internal("后台数据库任务失败"))?
    }
    async fn bind(&self, id: uuid::Uuid, run: DispatchedRun) -> Result<()> {
        self.db(move |s| {
            s.mutate(id, |t| {
                t.run_id = Some(run.id);
                t.run_attempt = run.attempt;
                t.run_url = Some(run.url);
                t.status = TaskStatus::Queued;
                t.error = None;
            })
        })
        .await
    }
    async fn defer_error(
        &self,
        id: uuid::Uuid,
        status: Option<TaskStatus>,
        error: Error,
        at: i64,
    ) -> Result<()> {
        let delay = match &error {
            Error::RateLimited(seconds) => i64::try_from(*seconds).unwrap_or(i64::MAX).max(15),
            _ => 15,
        };
        let message = error.to_string();
        self.db(move |s| {
            if let Some(status) = status {
                s.transition(id, status, Some(message))?;
            } else {
                s.mutate(id, |t| t.error = Some(message))?;
            }
            s.poll_after(id, at.saturating_add(delay))
        })
        .await
    }
    async fn result_error(
        &self,
        record: &TaskRecord,
        at: i64,
        successful_run: bool,
        error: Error,
    ) -> Result<()> {
        let status = if !successful_run {
            TaskStatus::Failed
        } else if at - record.created_at >= 86400 {
            TaskStatus::NeedsReview
        } else {
            TaskStatus::ResultPending
        };
        self.defer_error(record.id, Some(status), error, at).await
    }
    pub async fn tick(&self) -> Result<()> {
        let _guard = self.gate.lock().await;
        let at = self.clock.now();
        if let Some(record) = self.db(|s| s.active()).await? {
            if record.next_poll_at > at {
                return Ok(());
            }
            let id = record.id;
            self.db(move |s| s.poll_after(id, at + 15)).await?;
            if record.run_id.is_none()
                && matches!(
                    record.status,
                    TaskStatus::DispatchUnknown | TaskStatus::NeedsReview | TaskStatus::Dispatching
                )
            {
                match self
                    .api
                    .find_run(&record.connection, id, record.created_at)
                    .await
                {
                    Ok(Some(run)) => self.bind(id, run).await?,
                    Ok(None) => {
                        let status = if at - record.created_at >= 86400 {
                            TaskStatus::NeedsReview
                        } else {
                            TaskStatus::DispatchUnknown
                        };
                        self.db(move |s| {
                            s.transition(
                                id,
                                status,
                                Some("尚未找到对应 GitHub 运行，系统不会重复派发".into()),
                            )?;
                            s.poll_after(id, at + 60)
                        })
                        .await?;
                    }
                    Err(e) => {
                        let status = if matches!(e, Error::Conflict(_)) {
                            TaskStatus::NeedsReview
                        } else {
                            record.status
                        };
                        self.defer_error(id, Some(status), e, at).await?;
                    }
                }
                return Ok(());
            }
            let Some(run_id) = record.run_id else {
                self.db(move |s| {
                    s.transition(
                        id,
                        TaskStatus::DispatchUnknown,
                        Some("缺少运行标识，正在对账".into()),
                    )
                })
                .await?;
                return Ok(());
            };
            match self.api.get_run(&record.connection, run_id).await {
                Ok(run) => {
                    if run.attempt > record.run_attempt {
                        self.db(move |s| s.reopen_attempt(id, run.attempt)).await?;
                    }
                    match run.status.as_str() {
                        "queued" | "waiting" | "pending" | "requested" => {
                            self.db(move |s| s.transition(id, TaskStatus::Queued, None))
                                .await?
                        }
                        "in_progress" => {
                            self.db(move |s| s.transition(id, TaskStatus::Running, None))
                                .await?
                        }
                        "completed" => {
                            if run.conclusion.as_deref() == Some("cancelled") {
                                self.db(move |s| {
                                    s.transition(
                                        id,
                                        TaskStatus::Cancelled,
                                        Some("GitHub 任务已取消".into()),
                                    )
                                })
                                .await?;
                                return Ok(());
                            }
                            let result = self
                                .api
                                .download_result(&record.connection, run_id, run.attempt)
                                .await;
                            match result {
                                Ok(mut result) => {
                                    let secrets = self
                                        .db(move |s| Ok(payload_secrets(&s.payload(id)?)))
                                        .await?;
                                    for item in &mut result.items {
                                        item.error = item.error.take().map(|v| redact(v, &secrets));
                                    }
                                    if result.task_id != id {
                                        self.result_error(
                                            &record,
                                            at,
                                            run.conclusion.as_deref() == Some("success"),
                                            Error::invalid("结果 task_id 不匹配"),
                                        )
                                        .await?;
                                        return Ok(());
                                    }
                                    let applied = self.db(move |s| s.apply_result(&result)).await;
                                    if let Err(e) = applied {
                                        self.result_error(
                                            &record,
                                            at,
                                            run.conclusion.as_deref() == Some("success"),
                                            e,
                                        )
                                        .await?;
                                    } else if run.conclusion.as_deref() != Some("success") {
                                        self.db(move|s|s.transition(id,TaskStatus::Failed,Some("镜像步骤已返回结果，但 GitHub 工作流未成功结束".into()))).await?;
                                    }
                                }
                                Err(e) => {
                                    self.result_error(
                                        &record,
                                        at,
                                        run.conclusion.as_deref() == Some("success"),
                                        e,
                                    )
                                    .await?;
                                }
                            }
                        }
                        _ => {
                            self.db(move |s| {
                                s.mutate(id, |t| {
                                    t.error = Some("GitHub 返回暂不支持的状态，等待后续查询".into())
                                })
                            })
                            .await?
                        }
                    }
                }
                Err(e) => {
                    self.defer_error(id, None, e, at).await?;
                }
            }
            return Ok(());
        }
        if !self
            .db(|s| Ok(s.settings()?.is_some_and(|v| v.ready)))
            .await?
        {
            return Ok(());
        }
        let Some(record) = self.db(move |s| s.claim_next_at(at)).await? else {
            return Ok(());
        };
        let id = record.id;
        let envelope = self.db(move |s| s.envelope(id)).await?;
        match self
            .api
            .dispatch(
                &record.connection,
                id,
                &envelope,
                &self.store.crypto.task_secret_name(),
            )
            .await
        {
            Ok(run) => self.bind(id, run).await?,
            Err(DispatchError::Rejected) => {
                self.db(move |s| {
                    s.transition(
                        id,
                        TaskStatus::DispatchFailed,
                        Some("GitHub 拒绝派发，请检查 Token 权限、分支和工作流配置".into()),
                    )
                })
                .await?
            }
            Err(DispatchError::Unknown) => {
                self.db(move |s| {
                    s.transition(
                        id,
                        TaskStatus::DispatchUnknown,
                        Some("派发响应不确定，正在核实，不会自动重复发送".into()),
                    )
                })
                .await?
            }
            Err(DispatchError::RateLimited(seconds)) => {
                self.db(move |s| {
                    s.transition(
                        id,
                        TaskStatus::Pending,
                        Some("GitHub 限流，稍后继续".into()),
                    )?;
                    s.poll_after(id, at + seconds.max(15) as i64)
                })
                .await?
            }
        }
        Ok(())
    }
    pub async fn refresh_rules(&self) -> Result<()> {
        let at = self.clock.now();
        let ids = self
            .db(move |s| {
                let mut seen = std::collections::HashSet::new();
                let mut records = Vec::new();
                for rule in s.rules()? {
                    if let Some(id) = rule.sync.task_id.filter(|id| seen.insert(*id)) {
                        let record = s.record(id)?;
                        if record.status.terminal()
                            && record.run_id.is_some()
                            && record.next_poll_at <= at
                        {
                            records.push(record);
                        }
                    }
                }
                records.sort_by_key(|r| (r.next_poll_at, r.created_at));
                Ok(records
                    .into_iter()
                    .take(5)
                    .map(|r| r.id)
                    .collect::<Vec<_>>())
            })
            .await?;
        let deadline = rocket::tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        for id in ids {
            let outcome =
                rocket::tokio::time::timeout_at(deadline, self.refresh_terminal(id)).await;
            let error = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("GitHub 状态更新失败，显示上次记录：{e}")),
                Err(_) => Some("GitHub 状态查询超时，显示上次记录".into()),
            };
            self.db(move |s| s.set_refresh_error(id, error)).await?;
            if rocket::tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok(())
    }
    pub async fn refresh_terminal(&self, id: uuid::Uuid) -> Result<()> {
        let _guard = self.gate.lock().await;
        let at = self.clock.now();
        let record = self.db(move |s| s.record(id)).await?;
        if !record.status.terminal() || record.next_poll_at > at {
            return Ok(());
        }
        self.db(move |s| s.poll_after(id, at + 60)).await?;
        if let Some(run_id) = record.run_id {
            let run = self.api.get_run(&record.connection, run_id).await?;
            if run.attempt > record.run_attempt {
                self.db(move |s| s.reopen_attempt(id, run.attempt)).await?;
            }
        }
        Ok(())
    }
    pub async fn logs(&self, id: uuid::Uuid) -> Result<String> {
        let record = self.db(move |s| s.record(id)).await?;
        if let Some(logs) = self.db(move |s| s.logs(id, record.run_attempt)).await? {
            return Ok(logs);
        }
        if !record.status.terminal() {
            return Err(Error::invalid(
                "任务完成后可查看日志；运行中请打开 GitHub 链接",
            ));
        }
        let run_id = record
            .run_id
            .ok_or_else(|| Error::invalid("任务尚未关联 GitHub 运行"))?;
        let logs = self
            .api
            .download_logs(&record.connection, run_id, record.run_attempt)
            .await?;
        let mut secrets = self
            .db(move |s| Ok(payload_secrets(&s.payload(id)?)))
            .await?;
        secrets.push(self.db(|s| s.token()).await?);
        let logs = redact(logs, &secrets);
        let cached = logs.clone();
        self.db(move |s| s.save_logs(id, record.run_attempt, &cached))
            .await?;
        Ok(logs)
    }
}

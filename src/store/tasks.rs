use super::config::{RegistryRecord, RuleRecord};
use super::*;
use crate::domain::*;
use uuid::Uuid;

const TASK_WITH_RESULT: &str = "SELECT t.data,a.result FROM tasks t LEFT JOIN attempts a ON a.task_id=t.id AND a.attempt=json_extract(t.data,'$.run_attempt')";

fn task_view(data: String, result: Option<String>) -> Result<TaskView> {
    let mut view: TaskView = decode(data)?;
    view.elapsed_ms = result.map(decode::<BatchResult>).transpose()?.map(|r| {
        r.items
            .iter()
            .fold(0u64, |total, item| total.saturating_add(item.elapsed_ms))
    });
    Ok(view)
}

impl Store {
    pub fn task_page(&self, number: u64, size: u32) -> Result<Page<TaskView>> {
        if number == 0 || !(1..=100).contains(&size) {
            return Err(Error::invalid("页码需大于零，每页条数应为 1 至 100"));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let total: u64 = tx.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?;
        let pages = total.div_ceil(u64::from(size)).max(1);
        let page = number.min(pages);
        let offset = (page - 1) * u64::from(size);
        let mut query = tx.prepare(&format!(
            "{TASK_WITH_RESULT} ORDER BY t.rowid DESC LIMIT ?1 OFFSET ?2"
        ))?;
        let items = query
            .query_map(rusqlite::params![size, offset], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .map(|row| {
                let (data, result) = row?;
                task_view(data, result)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Page {
            items,
            total,
            page,
            page_size: size,
            pages,
        })
    }
    pub fn enqueue(&self, ids: &[Uuid], request_key: Uuid) -> Result<TaskView> {
        self.tx(|tx| {
            let previous:Option<String>=tx.query_row("SELECT data FROM tasks WHERE request_key=?1",[request_key.to_string()],|r|r.get(0)).optional()?;
            if let Some(previous)=previous {return decode(previous);}
            if ids.is_empty() || ids.len()>20 {return Err(Error::invalid("请选择 1 至 20 条规则"));}
            let setting:Option<String>=tx.query_row("SELECT data FROM settings WHERE id=1",[],|r|r.get(0)).optional()?;
            let raw:serde_json::Value=decode(setting.ok_or_else(||Error::invalid("请先配置 GitHub"))?)?;
            let view:SettingsView=serde_json::from_value(raw["view"].clone())?;
            if !view.ready {return Err(Error::invalid("请先在连接设置中完成 GitHub 初始化"));}
            let connection=ConnectionSnapshot{owner:view.owner,repo:view.repo,branch:view.branch,workflow:view.workflow};
            let task_id=Uuid::new_v4();let mut items=Vec::new();
            for id in ids {
                let rule=read::<RuleRecord>(tx,"rules",&id.to_string())?;
                let registry=read::<RegistryRecord>(tx,"registries",&rule.view.registry_id.to_string())?;
                if !rule.view.enabled || !registry.view.enabled {return Err(Error::invalid("选中的规则或仓库已停用"));}
                let locked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_targets WHERE target=?1)",[&rule.view.target],|r|r.get(0))?;
                if locked {return Err(Error::Conflict("目标已有未完成任务".into()));}
                items.push(SyncItem{item_id:*id,source:rule.view.source,target:rule.view.target,platform:rule.view.platform,
                    source_credentials:rule.source_password.map(|p| -> Result<Credentials> {Ok(Credentials::new(rule.view.source_username.unwrap_or_default(),self.crypto.open_secret(&p)?))}).transpose()?,
                    target_credentials:Credentials::new(registry.view.username,self.crypto.open_secret(&registry.password)?)});
            }
            let payload=TaskPayload{version:PROTOCOL_VERSION,task_id,items};
            let envelope=self.crypto.seal_task(&payload)?;
            let view=TaskView{refresh_error:None,id:task_id,status:TaskStatus::Pending,created_at:now(),updated_at:now(),run_id:None,run_attempt:1,run_url:None,error:None,
                items:payload.items.iter().map(|i|ItemView{item_id:i.item_id,source:i.source.clone(),target:i.target.clone(),platform:i.platform}).collect(),retry_of:None,elapsed_ms:None};
            tx.execute("INSERT INTO tasks(id,status,data,payload,connection,request_key) VALUES(?1,'pending',?2,?3,?4,?5)",rusqlite::params![task_id.to_string(),json(&view)?,envelope,json(&connection)?,request_key.to_string()])?;
            for item in &payload.items {tx.execute("INSERT INTO task_targets(target,task_id) VALUES(?1,?2)",rusqlite::params![item.target,task_id.to_string()])?;}
            Ok(view)
        })
    }
    pub fn task(&self, id: Uuid) -> Result<TaskView> {
        let row: Option<(String, Option<String>)> = self
            .conn()?
            .query_row(
                &format!("{TASK_WITH_RESULT} WHERE t.id=?1"),
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (data, result) = row.ok_or_else(|| Error::NotFound("记录不存在".into()))?;
        task_view(data, result)
    }
    pub fn tasks(&self) -> Result<Vec<TaskView>> {
        let conn = self.conn()?;
        let mut q = conn.prepare(&format!(
            "{TASK_WITH_RESULT} ORDER BY t.rowid DESC LIMIT 200"
        ))?;
        q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .map(|v| {
            let (data, result) = v?;
            task_view(data, result)
        })
        .collect()
    }
    pub fn record(&self, id: Uuid) -> Result<TaskRecord> {
        let conn = self.conn()?;
        Self::record_on(&conn, id)
    }
    fn record_on(conn: &Connection, id: Uuid) -> Result<TaskRecord> {
        let view: TaskView = read(conn, "tasks", &id.to_string())?;
        let (connection, next_poll_at): (String, i64) = conn.query_row(
            "SELECT connection,next_poll FROM tasks WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(TaskRecord {
            id,
            status: view.status,
            connection: decode(connection)?,
            created_at: view.created_at,
            run_id: view.run_id,
            run_attempt: view.run_attempt,
            next_poll_at,
        })
    }
    pub fn payload(&self, id: Uuid) -> Result<TaskPayload> {
        let value: String = self.conn()?.query_row(
            "SELECT payload FROM tasks WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        self.crypto.open_task(id, &value)
    }
    pub fn envelope(&self, id: Uuid) -> Result<String> {
        Ok(self.conn()?.query_row(
            "SELECT payload FROM tasks WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn active(&self) -> Result<Option<TaskRecord>> {
        let conn = self.conn()?;
        let id:Option<String>=conn.query_row("SELECT id FROM tasks WHERE status NOT IN ('pending','succeeded','failed','cancelled','dispatch_failed') ORDER BY rowid LIMIT 1",[],|r|r.get(0)).optional()?;
        id.map(|v| {
            Self::record_on(
                &conn,
                Uuid::parse_str(&v).map_err(|_| Error::Internal("任务标识损坏"))?,
            )
        })
        .transpose()
    }
    pub fn claim_next(&self) -> Result<Option<TaskRecord>> {
        self.claim_next_at(now())
    }
    pub fn claim_next_at(&self, at: i64) -> Result<Option<TaskRecord>> {
        self.tx(|tx| {
            let active:i64=tx.query_row("SELECT count(*) FROM tasks WHERE status NOT IN ('pending','succeeded','failed','cancelled','dispatch_failed')",[],|r|r.get(0))?;
            if active>0 {return Ok(None);}
            let row:Option<(String,i64)>=tx.query_row("SELECT data,next_poll FROM tasks WHERE status='pending' ORDER BY rowid LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((row,next_poll))=row else {return Ok(None)};
            if next_poll>at {return Ok(None);}
            let mut view:TaskView=decode(row)?;view.status=TaskStatus::Dispatching;view.updated_at=now();
            tx.execute("UPDATE tasks SET status='dispatching',data=?1 WHERE id=?2",rusqlite::params![json(&view)?,view.id.to_string()])?;
            Ok(Some(Self::record_on(tx,view.id)?))
        })
    }
    pub fn mutate(&self, id: Uuid, f: impl FnOnce(&mut TaskView)) -> Result<()> {
        self.tx(|tx| {
            let mut view: TaskView = read(tx, "tasks", &id.to_string())?;
            f(&mut view);
            view.updated_at = now();
            tx.execute(
                "UPDATE tasks SET status=?1,data=?2 WHERE id=?3",
                rusqlite::params![view.status.as_str(), json(&view)?, id.to_string()],
            )?;
            if view.status.terminal() {
                tx.execute(
                    "DELETE FROM task_targets WHERE task_id=?1",
                    [id.to_string()],
                )?;
            }
            Ok(())
        })
    }
    pub fn set_refresh_error(&self, id: Uuid, error: Option<String>) -> Result<()> {
        self.tx(|tx| {
            let mut view: TaskView = read(tx, "tasks", &id.to_string())?;
            view.refresh_error = error;
            tx.execute(
                "UPDATE tasks SET data=?1 WHERE id=?2",
                rusqlite::params![json(&view)?, id.to_string()],
            )?;
            Ok(())
        })
    }
    pub fn transition(&self, id: Uuid, status: TaskStatus, error: Option<String>) -> Result<()> {
        self.mutate(id, |t| {
            t.status = status;
            t.error = error
        })
    }
    pub fn recover_dispatching(&self) -> Result<()> {
        let conn = self.conn()?;
        let mut q = conn.prepare("SELECT id FROM tasks WHERE status='dispatching'")?;
        let ids: Vec<String> = q
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in ids {
            self.transition(
                Uuid::parse_str(&id).unwrap(),
                TaskStatus::DispatchUnknown,
                Some("服务重启，正在核实派发结果".into()),
            )?;
        }
        Ok(())
    }
    pub fn poll_after(&self, id: Uuid, at: i64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE tasks SET next_poll=?1 WHERE id=?2",
            rusqlite::params![at, id.to_string()],
        )?;
        Ok(())
    }
    pub fn apply_result(&self, result: &BatchResult) -> Result<()> {
        let task = self.task(result.task_id)?;
        if result.version != PROTOCOL_VERSION
            || result.run_attempt != task.run_attempt
            || result.items.len() != task.items.len()
        {
            return Err(Error::invalid("任务结果版本、尝试或条目数不匹配"));
        }
        let mut ids = std::collections::HashSet::new();
        for item in &result.items {
            if !ids.insert(item.item_id)
                || !task.items.iter().any(|v| {
                    v.item_id == item.item_id
                        && v.source == item.source
                        && v.target == item.target
                        && v.platform == item.platform
                })
            {
                return Err(Error::invalid("结果条目与任务不匹配"));
            }
            if item.outcome != ItemOutcome::Failed
                && (item.source_digest.is_none() || item.source_digest != item.target_digest)
            {
                return Err(Error::invalid("成功条目缺少一致的镜像 digest"));
            }
            if [
                &item.source_index_digest,
                &item.source_digest,
                &item.target_digest,
            ]
            .into_iter()
            .flatten()
            .any(|d| !crate::reference::valid_digest(d))
            {
                return Err(Error::invalid("任务结果包含无效 digest"));
            }
        }
        let failed = result
            .items
            .iter()
            .filter(|i| i.outcome == ItemOutcome::Failed)
            .count();
        let synced = result
            .items
            .iter()
            .filter(|i| i.outcome == ItemOutcome::Synced)
            .count();
        if result.summary.failed != failed
            || result.summary.synced != synced
            || result.summary.unchanged != result.items.len() - failed - synced
        {
            return Err(Error::invalid("结果汇总不一致"));
        }
        self.conn()?.execute("INSERT INTO attempts(task_id,attempt,result) VALUES(?1,?2,?3) ON CONFLICT(task_id,attempt) DO UPDATE SET result=excluded.result",rusqlite::params![result.task_id.to_string(),result.run_attempt,json(result)?])?;
        self.transition(
            result.task_id,
            if failed > 0 {
                TaskStatus::Failed
            } else {
                TaskStatus::Succeeded
            },
            None,
        )
    }
    pub fn result(&self, id: Uuid, attempt: u32) -> Result<Option<BatchResult>> {
        let value: Option<String> = self
            .conn()?
            .query_row(
                "SELECT result FROM attempts WHERE task_id=?1 AND attempt=?2",
                rusqlite::params![id.to_string(), attempt],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        value.map(decode).transpose()
    }
    pub fn logs(&self, id: Uuid, attempt: u32) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT logs FROM attempts WHERE task_id=?1 AND attempt=?2",
                rusqlite::params![id.to_string(), attempt],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn save_logs(&self, id: Uuid, attempt: u32, logs: &str) -> Result<()> {
        self.conn()?.execute("INSERT INTO attempts(task_id,attempt,logs) VALUES(?1,?2,?3) ON CONFLICT(task_id,attempt) DO UPDATE SET logs=excluded.logs",rusqlite::params![id.to_string(),attempt,logs])?;
        Ok(())
    }
    pub fn release_unknown(&self, id: Uuid) -> Result<()> {
        let status = self.task(id)?.status;
        if !matches!(
            status,
            TaskStatus::NeedsReview | TaskStatus::DispatchUnknown
        ) {
            return Err(Error::Conflict("仅未知派发任务可人工解除".into()));
        }
        self.transition(
            id,
            TaskStatus::Cancelled,
            Some("管理员确认解除；GitHub 可能仍在执行，请到 GitHub 核实".into()),
        )
    }
    pub fn reopen_attempt(&self, id: Uuid, attempt: u32) -> Result<()> {
        self.tx(|tx| {
            let mut view: TaskView = read(tx, "tasks", &id.to_string())?;
            if attempt <= view.run_attempt {
                return Ok(());
            }
            view.run_attempt = attempt;
            view.status = TaskStatus::Queued;
            view.error = None;
            view.updated_at = now();
            for item in &view.items {
                // 手工重跑可能与已排队任务引用同目标；保持全局队列阻塞并核实该运行。
                tx.execute(
                    "INSERT OR IGNORE INTO task_targets(target,task_id) VALUES(?1,?2)",
                    rusqlite::params![item.target, id.to_string()],
                )?;
            }
            tx.execute(
                "UPDATE tasks SET status='queued',data=?1,next_poll=0 WHERE id=?2",
                rusqlite::params![json(&view)?, id.to_string()],
            )?;
            Ok(())
        })
    }
    pub fn retry(&self, id: Uuid) -> Result<TaskView> {
        let old = self.task(id)?;
        if !old.status.terminal() {
            return Err(Error::Conflict("任务尚未结束，不能重试".into()));
        }
        let ids: Vec<_> = old.items.iter().map(|i| i.item_id).collect();
        let task = self.enqueue(&ids, Uuid::new_v4())?;
        self.mutate(task.id, |v| v.retry_of = Some(id))?;
        self.task(task.id)
    }
}

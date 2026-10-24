use super::*;
use reqwest::Method;
use serde_json::{Value, json};

fn run_from(value: &Value, c: &ConnectionSnapshot) -> Result<DispatchedRun> {
    let id = value["id"]
        .as_u64()
        .ok_or_else(|| Error::invalid("运行信息缺少 ID"))?;
    Ok(DispatchedRun {
        id,
        url: format!(
            "https://github.com/{}/{}/actions/runs/{id}",
            c.owner, c.repo
        ),
        attempt: value["run_attempt"].as_u64().unwrap_or(1) as u32,
    })
}
#[async_trait]
impl GithubApi for GithubClient {
    async fn dispatch(
        &self,
        c: &ConnectionSnapshot,
        id: Uuid,
        envelope: &str,
        secret_name: &str,
    ) -> std::result::Result<DispatchedRun, DispatchError> {
        let request = self
            .request(
                Method::POST,
                &format!(
                    "{}/actions/workflows/{}/dispatches",
                    Self::prefix(c),
                    c.workflow
                ),
            )
            .await
            .map_err(|_| DispatchError::Rejected)?;
        let result=request.json(&json!({"ref":c.branch,"inputs":{"task_id":id.to_string(),"version":"1","payload":envelope,"secret_name":secret_name}})).send().await.map_err(|_|DispatchError::Unknown)?;
        let code = result.status().as_u16();
        if code == 429
            || (code == 403
                && (result.headers().contains_key("retry-after")
                    || result
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|v| v == "0")))
        {
            let delay = result
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(60);
            return Err(DispatchError::RateLimited(delay));
        }
        if code != 200 {
            return Err(if code >= 500 || code == 408 {
                DispatchError::Unknown
            } else {
                DispatchError::Rejected
            });
        }
        let body = Self::json_response(result)
            .await
            .map_err(|_| DispatchError::Unknown)?;
        let run_id = body["workflow_run_id"]
            .as_u64()
            .ok_or(DispatchError::Unknown)?;
        Ok(DispatchedRun {
            id: run_id,
            url: format!(
                "https://github.com/{}/{}/actions/runs/{run_id}",
                c.owner, c.repo
            ),
            attempt: 1,
        })
    }
    async fn get_run(&self, c: &ConnectionSnapshot, id: u64) -> Result<RunState> {
        let result = self
            .request(
                Method::GET,
                &format!("{}/actions/runs/{id}", Self::prefix(c)),
            )
            .await?
            .send()
            .await
            .map_err(|_| Error::External("无法查询 GitHub 运行状态".into()))?;
        let value = Self::json_response(result).await?;
        if value["id"].as_u64() != Some(id) {
            return Err(Error::invalid("GitHub 返回了不匹配的运行 ID"));
        }
        let attempt = value["run_attempt"].as_u64().unwrap_or(1) as u32;
        Ok(RunState {
            id,
            attempt,
            status: value["status"].as_str().unwrap_or("unknown").into(),
            conclusion: value["conclusion"].as_str().map(Into::into),
        })
    }
    async fn find_run(
        &self,
        c: &ConnectionSnapshot,
        id: Uuid,
        since: i64,
    ) -> Result<Option<DispatchedRun>> {
        let title = format!("image-sync-{id}");
        let mut matched = Vec::new();
        let since = chrono::DateTime::from_timestamp(since.saturating_sub(60), 0)
            .unwrap_or_default()
            .to_rfc3339();
        for page in 1..=20 {
            let result = self
                .request(
                    Method::GET,
                    &format!("{}/actions/workflows/{}/runs", Self::prefix(c), c.workflow),
                )
                .await?
                .query(&[
                    ("event", "workflow_dispatch"),
                    ("branch", c.branch.as_str()),
                    ("created", &format!(">={since}")),
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                ])
                .send()
                .await
                .map_err(|_| Error::External("无法核实 GitHub 派发结果".into()))?;
            let value = Self::json_response(result).await?;
            let rows = value["workflow_runs"]
                .as_array()
                .ok_or_else(|| Error::invalid("运行列表格式错误"))?;
            for row in rows {
                if row["display_title"] == title
                    && row["event"] == "workflow_dispatch"
                    && row["head_branch"] == c.branch
                {
                    matched.push(run_from(row, c)?);
                }
            }
            if rows.len() < 100 {
                return match matched.len() {
                    0 => Ok(None),
                    1 => Ok(matched.pop()),
                    _ => Err(Error::Conflict("发现多个同名运行，需要人工核实".into())),
                };
            }
        }
        Err(Error::External("运行列表超过查询范围，请人工核实".into()))
    }
    async fn download_result(
        &self,
        c: &ConnectionSnapshot,
        id: u64,
        attempt: u32,
    ) -> Result<BatchResult> {
        let url = format!("{}/actions/runs/{id}/artifacts", Self::prefix(c));
        let response = self
            .request(Method::GET, &url)
            .await?
            .query(&[("per_page", "100")])
            .send()
            .await
            .map_err(|_| Error::External("无法查询任务结果".into()))?;
        let value = Self::json_response(response).await?;
        let name = format!("image-sync-result-{attempt}");
        let artifacts = value["artifacts"]
            .as_array()
            .ok_or_else(|| Error::invalid("结果列表无效"))?;
        let matches: Vec<_> = artifacts
            .iter()
            .filter(|a| a["name"] == name && a["expired"] != true)
            .collect();
        if matches.len() != 1 {
            return Err(Error::NotFound("任务结果尚未生成、已过期或存在歧义".into()));
        }
        let artifact_id = matches[0]["id"]
            .as_u64()
            .ok_or_else(|| Error::invalid("结果标识无效"))?;
        let entries = self
            .download_archive(&format!(
                "{}/actions/artifacts/{artifact_id}/zip",
                Self::prefix(c)
            ))
            .await?;
        let bytes = entries
            .get("result.json")
            .ok_or_else(|| Error::invalid("结果文件缺失"))?;
        let result: BatchResult = serde_json::from_slice(bytes)?;
        if result.run_attempt != attempt {
            return Err(Error::invalid("结果尝试次数不匹配"));
        }
        Ok(result)
    }
    async fn download_logs(&self, c: &ConnectionSnapshot, id: u64, attempt: u32) -> Result<String> {
        let entries = self
            .download_archive(&format!(
                "{}/actions/runs/{id}/attempts/{attempt}/logs",
                Self::prefix(c)
            ))
            .await?;
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let logs = entries
            .into_iter()
            .map(|(name, bytes)| format!("--- {name} ---\n{}\n", String::from_utf8_lossy(&bytes)))
            .collect::<String>();
        Ok(logs.chars().take(500_000).collect())
    }
}

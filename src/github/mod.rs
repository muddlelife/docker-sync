mod dispatch;
mod downloads;
mod setup;
use crate::{
    domain::*,
    error::{Error, Result},
    store::Store,
};
use async_trait::async_trait;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchedRun {
    pub id: u64,
    pub url: String,
    pub attempt: u32,
}
#[derive(Clone, Debug)]
pub struct RunState {
    pub id: u64,
    pub attempt: u32,
    pub status: String,
    pub conclusion: Option<String>,
}
#[derive(Debug)]
pub enum DispatchError {
    Rejected,
    Unknown,
    RateLimited(u64),
}
#[async_trait]
pub trait GithubApi: Send + Sync {
    async fn dispatch(
        &self,
        connection: &ConnectionSnapshot,
        id: Uuid,
        envelope: &str,
        secret_name: &str,
    ) -> std::result::Result<DispatchedRun, DispatchError>;
    async fn get_run(&self, connection: &ConnectionSnapshot, id: u64) -> Result<RunState>;
    async fn find_run(
        &self,
        connection: &ConnectionSnapshot,
        id: Uuid,
        since: i64,
    ) -> Result<Option<DispatchedRun>>;
    async fn download_result(
        &self,
        connection: &ConnectionSnapshot,
        id: u64,
        attempt: u32,
    ) -> Result<BatchResult>;
    async fn download_logs(
        &self,
        connection: &ConnectionSnapshot,
        id: u64,
        attempt: u32,
    ) -> Result<String>;
}
#[derive(Clone)]
pub struct GithubClient {
    client: Client,
    download_client: Client,
    base: String,
    token: Arc<dyn Fn() -> Result<String> + Send + Sync>,
}
impl GithubClient {
    pub(crate) fn using_token(&self, token: String) -> Self {
        let mut client = self.clone();
        client.token = Arc::new(move || Ok(token.clone()));
        client
    }
    pub fn new(token: String, base: &str) -> Result<Self> {
        Self::with_provider(Arc::new(move || Ok(token.clone())), base)
    }
    pub fn for_store(store: Store) -> Result<Self> {
        Self::with_provider(Arc::new(move || store.token()), "https://api.github.com")
    }
    fn with_provider(
        token: Arc<dyn Fn() -> Result<String> + Send + Sync>,
        base: &str,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("docker-sync/0.1")
            .build()
            .map_err(|_| Error::Internal("HTTP 客户端初始化失败"))?;
        Ok(Self {
            download_client: client.clone(),
            client,
            base: base.trim_end_matches('/').into(),
            token,
        })
    }
    pub(super) async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let token = self.token.clone();
        let token = rocket::tokio::task::spawn_blocking(move || token())
            .await
            .map_err(|_| Error::Internal("无法读取 GitHub 凭据"))??;
        Ok(self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10"))
    }
    pub(super) fn prefix(c: &ConnectionSnapshot) -> String {
        format!("/repos/{}/{}", c.owner, c.repo)
    }
    pub(super) async fn bytes(mut response: Response, limit: usize) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::External("GitHub 响应读取失败".into()))?
        {
            if data.len() + chunk.len() > limit {
                return Err(Error::External("GitHub 响应超过大小限制".into()));
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }
    pub(super) async fn json_response(response: Response) -> Result<serde_json::Value> {
        if response.status().as_u16() == 429
            || (response.status().as_u16() == 403
                && (response.headers().contains_key("retry-after")
                    || response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .is_some_and(|v| v == "0")))
        {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            return Err(Error::RateLimited(delay));
        }
        if !response.status().is_success() {
            return Err(Error::External(format!(
                "GitHub API 返回 {}，请检查权限或稍后重试",
                response.status().as_u16()
            )));
        }
        Ok(serde_json::from_slice(
            &Self::bytes(response, 2 * 1024 * 1024).await?,
        )?)
    }
    pub async fn check_workflow(&self, c: &ConnectionSnapshot) -> Result<()> {
        let path = format!("{}/actions/workflows/{}", Self::prefix(c), c.workflow);
        let result = self
            .request(Method::GET, &path)
            .await?
            .send()
            .await
            .map_err(|_| Error::External("无法连接 GitHub".into()))?;
        match result.status().as_u16() {
            401 => return Err(Error::invalid("GitHub Token 无效或已过期")),
            404 => {
                return Err(Error::invalid(
                    "找不到同步工作流，请确认 Token 可以访问该仓库，并上传 web-sync.yml 到默认分支",
                ));
            }
            403 if !result.headers().contains_key("retry-after")
                && !result
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0") =>
            {
                return Err(Error::invalid(
                    "无法访问同步工作流，请授予该执行仓库的 Actions 读写权限",
                ));
            }
            _ => {}
        }
        let json = Self::json_response(result).await?;
        if json["state"] != "active" {
            return Err(Error::invalid(
                "工作流未启用，请检查仓库默认分支中的 web-sync.yml",
            ));
        }
        if json["name"] != "Web 镜像同步 v2" {
            return Err(Error::invalid(
                "工作流版本过旧，请下载最新 web-sync.yml 并更新执行仓库",
            ));
        }
        self.check_workflow_contents(c).await
    }
}

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use crypto_box::PublicKey;
use serde_json::json;

impl GithubClient {
    pub(super) async fn check_workflow_contents(&self, c: &ConnectionSnapshot) -> Result<()> {
        let path = format!(
            "{}/contents/.github/workflows/{}",
            Self::prefix(c),
            c.workflow
        );
        let mut raw_headers = reqwest::header::HeaderMap::new();
        raw_headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.github.raw+json"),
        );
        let mut response = self
            .request(Method::GET, &path)
            .await?
            .headers(raw_headers)
            .query(&[("ref", c.branch.as_str())])
            .send()
            .await
            .map_err(|_| Error::External("无法读取执行分支的同步工作流".into()))?;
        let limited = |r: &Response| {
            r.status() == 429
                || r.headers().contains_key("retry-after")
                || r.headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0")
        };
        if matches!(response.status().as_u16(), 403 | 404) && !limited(&response) {
            // Public repositories can be read without expanding the Token's permissions.
            response = self
                .client
                .get(format!("{}{path}", self.base))
                .header("Accept", "application/vnd.github.raw+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .query(&[("ref", c.branch.as_str())])
                .send()
                .await
                .map_err(|_| Error::External("无法读取执行分支的同步工作流".into()))?;
        }
        if limited(&response) {
            return Self::json_response(response).await.map(|_| ());
        }
        if !response.status().is_success() {
            return Err(Error::invalid(
                "无法读取执行分支的工作流：请确认该分支包含 web-sync.yml；私有仓库还需授予 Token 的 Contents 只读权限",
            ));
        }
        let bytes = Self::bytes(response, 128 * 1024).await?;
        let actual = std::str::from_utf8(&bytes)
            .map_err(|_| Error::invalid("同步工作流编码无效"))?
            .replace("\r\n", "\n");
        let expected = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/workflows/web-sync.yml"
        ))
        .replace("\r\n", "\n");
        if actual.trim() != expected.trim() {
            return Err(Error::invalid(
                "执行分支的工作流不匹配，请下载最新 web-sync.yml 并原样更新到该分支",
            ));
        }
        Ok(())
    }
    pub async fn initialize(
        &self,
        c: &ConnectionSnapshot,
        secret_name: &str,
        key: &str,
    ) -> Result<()> {
        if !secret_name.starts_with("DOCKER_SYNC_KEY_")
            || !secret_name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::invalid("任务密钥标识无效"));
        }
        crate::crypto::Crypto::key_from_hex(key)?;
        self.check_workflow(c).await?;
        let response = self
            .request(
                Method::GET,
                &format!("{}/actions/secrets/public-key", Self::prefix(c)),
            )
            .await?
            .send()
            .await
            .map_err(|_| Error::External("无法获取 GitHub 加密公钥".into()))?;
        setup_status(&response)?;
        let public = Self::json_response(response).await?;
        let public_key = public["key"]
            .as_str()
            .and_then(|v| STANDARD.decode(v).ok())
            .and_then(|v| PublicKey::from_slice(&v).ok())
            .ok_or_else(|| Error::External("GitHub 加密公钥格式不正确".into()))?;
        let key_id = public["key_id"]
            .as_str()
            .ok_or_else(|| Error::External("GitHub 公钥标识缺失".into()))?;
        let cipher = public_key
            .seal(&mut rand::rngs::OsRng, key.as_bytes())
            .map_err(|_| Error::Internal("任务密钥加密失败"))?;
        let response = self
            .request(
                Method::PUT,
                &format!("{}/actions/secrets/{secret_name}", Self::prefix(c)),
            )
            .await?
            .json(&json!({"key_id":key_id,"encrypted_value":STANDARD.encode(cipher)}))
            .send()
            .await
            .map_err(|_| Error::External("无法自动配置 GitHub Secret，请重试初始化".into()))?;
        setup_status(&response)?;
        if !matches!(response.status().as_u16(), 201 | 204) {
            return Err(Error::External(format!(
                "GitHub Secret 配置失败（HTTP {}）",
                response.status().as_u16()
            )));
        }
        Ok(())
    }
}
fn setup_status(response: &Response) -> Result<()> {
    match response.status().as_u16() {
        401 => Err(Error::invalid("GitHub Token 无效或已过期，请更新 Token")),
        403 | 404
            if !response.headers().contains_key("retry-after")
                && !response
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0") =>
        {
            Err(Error::invalid(
                "无法配置 GitHub Secret，请为该执行仓库授予 Token 的 Secrets 读写权限",
            ))
        }
        _ => Ok(()),
    }
}

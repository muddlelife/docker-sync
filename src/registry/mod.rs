mod auth;
mod images;
#[cfg(test)]
mod tests;

use crate::{
    domain::*,
    error::{Error, Result},
    reference::{normalize_endpoint, valid_repository, valid_tag},
};
use reqwest::{Client, Response};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
    time::Duration,
};
use url::Url;

const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 5;
const MAX_REPOSITORIES: usize = 30;
const MAX_IMAGES: usize = 100;
const MAX_JSON: usize = 1024 * 1024;
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

pub struct RegistryClient {
    client: Client,
    origin: Url,
    credentials: Credentials,
    tokens: Mutex<HashMap<String, String>>,
}
impl RegistryClient {
    pub fn new(endpoint: &str, credentials: Credentials) -> Result<Self> {
        let endpoint = normalize_endpoint(endpoint)?;
        Self::with_base(
            Url::parse(&format!("https://{endpoint}/"))
                .map_err(|_| Error::invalid("仓库地址无效"))?,
            credentials,
        )
    }
    fn with_base(origin: Url, credentials: Credentials) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .user_agent("docker-sync/0.1")
            .build()
            .map_err(|_| Error::Internal("仓库客户端初始化失败"))?;
        Ok(Self {
            client,
            origin,
            credentials,
            tokens: Mutex::new(HashMap::new()),
        })
    }
    pub async fn browse(
        &self,
        registry: &RegistryView,
        known: &[String],
        repository: Option<&str>,
    ) -> Result<InventoryView> {
        if repository.is_some_and(|r| r.len() > 255 || !valid_repository(r)) {
            return Err(Error::invalid("请输入镜像名称，不含协议、标签或相对路径"));
        }
        rocket::tokio::time::timeout(
            Duration::from_secs(40),
            self.browse_inner(registry, known, repository),
        )
        .await
        .map_err(|_| Error::External("仓库查询超时，请指定一个镜像名称后重试".into()))?
    }
    async fn browse_inner(
        &self,
        registry: &RegistryView,
        known: &[String],
        repository: Option<&str>,
    ) -> Result<InventoryView> {
        let mut view = InventoryView {
            checked_at: crate::store::now(),
            scope: "catalog".into(),
            complete: true,
            repositories: Vec::new(),
            warnings: Vec::new(),
            images: Vec::new(),
        };
        if let Some(repository) = repository {
            view.scope = "repository".into();
            view.repositories.push(repository.into());
        } else {
            match self
                .pages("/v2/_catalog", "repositories", "registry:catalog:*", true)
                .await?
            {
                Some((names, complete)) => {
                    view.complete = complete;
                    let prefix = format!("{}/", registry.namespace);
                    view.repositories = names
                        .into_iter()
                        .filter_map(|n| n.strip_prefix(&prefix).map(str::to_owned))
                        .filter(|n| valid_repository(n))
                        .collect();
                }
                None => {
                    view.scope = "known".into();
                    view.complete = false;
                    view.warnings.push("仓库不支持完整目录查询，当前仅查询已配置规则中的镜像；其他镜像可按名称查询。".into());
                    view.repositories = known
                        .iter()
                        .filter(|n| valid_repository(n))
                        .cloned()
                        .collect();
                }
            }
        }
        view.repositories.sort();
        view.repositories.dedup();
        if view.repositories.len() > MAX_REPOSITORIES {
            view.repositories.truncate(MAX_REPOSITORIES);
            view.complete = false;
        }
        for repository in &view.repositories {
            if view.images.len() >= MAX_IMAGES {
                view.complete = false;
                break;
            }
            let name = format!("{}/{repository}", registry.namespace);
            let scope = format!("repository:{name}:pull");
            let listed = self
                .pages(&format!("/v2/{name}/tags/list"), "tags", &scope, false)
                .await;
            let (tags, complete) = match listed {
                Ok(Some(result)) => result,
                Ok(None) => unreachable!(),
                Err(e) if view.scope == "repository" => return Err(e),
                Err(e) => {
                    view.complete = false;
                    view.warnings.push(format!("{repository}：{e}"));
                    continue;
                }
            };
            view.complete &= complete;
            for tag in tags {
                if view.images.len() >= MAX_IMAGES {
                    view.complete = false;
                    break;
                }
                if !valid_tag(&tag) {
                    view.complete = false;
                    view.warnings
                        .push(format!("{repository}：仓库返回无效标签，已跳过"));
                    continue;
                }
                let mut image = RemoteImage {
                    reference: format!("{}/{name}:{tag}", registry.endpoint),
                    repository: repository.clone(),
                    tag: tag.clone(),
                    platforms: Vec::new(),
                    digest: None,
                    error: None,
                };
                match self.describe(&name, &tag, &scope).await {
                    Ok((digest, platforms)) => {
                        image.digest = Some(digest);
                        image.platforms = platforms;
                        if image.platforms.is_empty() {
                            image.error = Some("镜像未提供可识别的架构信息".into());
                            view.complete = false;
                        }
                    }
                    Err(e) => {
                        image.error = Some(e.to_string());
                        view.complete = false;
                    }
                }
                view.images.push(image);
            }
        }
        if !view.complete && view.scope != "known" && view.warnings.is_empty() {
            view.warnings.push(
                "查询结果不完整：部分元数据读取失败或已达到查询上限。可指定镜像名称缩小范围。"
                    .into(),
            );
        }
        Ok(view)
    }
    async fn pages(
        &self,
        path: &str,
        key: &str,
        scope: &str,
        catalog: bool,
    ) -> Result<Option<(Vec<String>, bool)>> {
        let mut url = self
            .origin
            .join(path)
            .map_err(|_| Error::invalid("仓库查询路径无效"))?;
        url.query_pairs_mut()
            .append_pair("n", &PAGE_SIZE.to_string());
        let mut visited = BTreeSet::new();
        let mut items = BTreeSet::new();
        for _ in 0..MAX_PAGES {
            if !visited.insert(url.to_string()) {
                return Err(Error::External("仓库分页链接重复，已停止查询".into()));
            }
            let response = self.request(&url, scope).await?;
            if catalog && matches!(response.status().as_u16(), 401 | 403 | 404 | 405 | 501) {
                return Ok(None);
            }
            let response = check(response)?;
            let next = next_page(&response, &url, &self.origin, path)?;
            let json: Value = serde_json::from_slice(&body(response, MAX_JSON).await?)
                .map_err(|_| Error::External("仓库返回的数据格式不正确".into()))?;
            if let Some(rows) = json[key].as_array() {
                if rows.len() > 1000 {
                    return Err(Error::External("仓库单页条目超过限制".into()));
                }
                for row in rows {
                    items.insert(
                        row.as_str()
                            .ok_or_else(|| Error::External("仓库列表格式不正确".into()))?
                            .to_owned(),
                    );
                }
            } else if !(key == "tags" && json.get(key).is_some_and(Value::is_null)) {
                return Err(Error::External("仓库列表缺少必要字段".into()));
            }
            if let Some(next) = next {
                url = next;
            } else {
                return Ok(Some((items.into_iter().collect(), true)));
            }
        }
        Ok(Some((items.into_iter().collect(), false)))
    }
}
fn next_page(response: &Response, current: &Url, origin: &Url, path: &str) -> Result<Option<Url>> {
    let Some(link) = response.headers().get("link") else {
        return Ok(None);
    };
    let link = link
        .to_str()
        .map_err(|_| Error::External("仓库分页链接无效".into()))?;
    for part in link.split(',') {
        if !part.contains("rel=\"next\"") && !part.contains("rel=next") {
            continue;
        }
        let start = part
            .find('<')
            .ok_or_else(|| Error::External("仓库分页链接无效".into()))?;
        let end = part[start + 1..]
            .find('>')
            .map(|n| n + start + 1)
            .ok_or_else(|| Error::External("仓库分页链接无效".into()))?;
        let url = current
            .join(&part[start + 1..end])
            .map_err(|_| Error::External("仓库分页链接无效".into()))?;
        if url.origin() != origin.origin()
            || url.path() != path
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::External(
                "仓库返回不可信的分页地址，已停止查询".into(),
            ));
        }
        return Ok(Some(url));
    }
    Ok(None)
}
fn check(response: Response) -> Result<Response> {
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(Error::External(
            "仓库拒绝访问，请检查用户名、密码及读取权限".into(),
        )),
        404 => Err(Error::NotFound("远端镜像或标签不存在".into())),
        429 => Err(Error::RateLimited(60)),
        300..=399 => Err(Error::External(
            "仓库返回重定向，已停止携带凭据的请求".into(),
        )),
        code => Err(Error::External(format!(
            "仓库暂时不可用（HTTP {code}），请稍后重试"
        ))),
    }
}
async fn body(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::External("仓库响应读取失败".into()))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(Error::External("仓库响应超过大小限制".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

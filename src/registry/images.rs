use super::*;
use sha2::{Digest, Sha256};
impl RegistryClient {
    pub(super) async fn describe(
        &self,
        name: &str,
        tag: &str,
        scope: &str,
    ) -> Result<(String, Vec<String>)> {
        let (manifest, raw) = self.manifest(name, tag, scope).await?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&raw)));
        let mut pending = vec![(manifest, 0)];
        let mut platforms = BTreeSet::new();
        let mut fetched = 0;
        while let Some((manifest, depth)) = pending.pop() {
            if let Some(descriptors) = manifest.get("manifests").and_then(Value::as_array) {
                if descriptors.len() > 64 || depth >= 3 {
                    return Err(Error::External("镜像架构索引超过查询限制".into()));
                }
                for descriptor in descriptors {
                    if descriptor["annotations"]["vnd.docker.reference.type"]
                        == "attestation-manifest"
                        || descriptor.get("artifactType").is_some()
                    {
                        continue;
                    }
                    let digest = descriptor["digest"]
                        .as_str()
                        .filter(|s| crate::reference::valid_digest(s))
                        .ok_or_else(|| Error::External("镜像架构描述缺少有效摘要".into()))?;
                    if let Some(platform) = platform(&descriptor["platform"]) {
                        platforms.insert(platform);
                    } else {
                        fetched += 1;
                        if fetched > 32 {
                            return Err(Error::External("镜像架构索引超过查询限制".into()));
                        }
                        pending.push((self.manifest(name, digest, scope).await?.0, depth + 1));
                    }
                }
            } else if let Some(digest) = manifest["config"]["digest"]
                .as_str()
                .filter(|s| crate::reference::valid_digest(s))
            {
                let url = self
                    .origin
                    .join(&format!("/v2/{name}/blobs/{digest}"))
                    .map_err(|_| Error::invalid("镜像配置地址无效"))?;
                let response = self.request(&url, scope).await?;
                let response = if response.status().is_redirection() {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| url.join(v).ok())
                        .ok_or_else(|| Error::External("镜像配置下载地址无效".into()))?;
                    let host = location.host_str().unwrap_or("");
                    let storage = [
                        ".aliyuncs.com",
                        ".myqcloud.com",
                        ".volces.com",
                        ".amazonaws.com",
                        ".blob.core.windows.net",
                    ]
                    .iter()
                    .any(|suffix| host.ends_with(suffix));
                    if location.scheme() != "https"
                        || !location.username().is_empty()
                        || location.password().is_some()
                        || (!storage && location.origin() != self.origin.origin())
                    {
                        return Err(Error::External("镜像配置下载地址不可信".into()));
                    }
                    // Signed storage redirects are followed explicitly, without registry credentials.
                    self.client
                        .get(location)
                        .send()
                        .await
                        .map_err(|_| Error::External("镜像配置下载失败".into()))?
                } else {
                    response
                };
                let config: Value =
                    serde_json::from_slice(&body(check(response)?, MAX_JSON).await?)
                        .map_err(|_| Error::External("镜像配置格式不正确".into()))?;
                if let Some(p) = platform(&config) {
                    platforms.insert(p);
                } else {
                    return Err(Error::External("部分镜像架构无法识别".into()));
                }
            } else {
                return Err(Error::External("镜像未提供可识别的架构配置".into()));
            }
        }
        Ok((digest, platforms.into_iter().collect()))
    }
    async fn manifest(&self, name: &str, reference: &str, scope: &str) -> Result<(Value, Vec<u8>)> {
        let url = self
            .origin
            .join(&format!("/v2/{name}/manifests/{reference}"))
            .map_err(|_| Error::invalid("镜像索引地址无效"))?;
        let bytes = body(check(self.request(&url, scope).await?)?, MAX_JSON).await?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::External("镜像索引格式不正确".into()))?;
        if manifest["schemaVersion"] != 2 {
            return Err(Error::External("镜像索引版本暂不支持".into()));
        }
        Ok((manifest, bytes))
    }
}
fn platform(value: &Value) -> Option<String> {
    let os = value["os"].as_str()?;
    let arch = value["architecture"].as_str()?;
    let valid = |s: &str| {
        !s.is_empty()
            && s != "unknown"
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    };
    if !valid(os) || !valid(arch) {
        return None;
    }
    let mut name = format!("{os}/{arch}");
    if let Some(variant) = value["variant"].as_str().filter(|s| valid(s)) {
        name.push('/');
        name.push_str(variant);
    }
    Some(name)
}

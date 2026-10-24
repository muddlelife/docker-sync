use super::*;
use regex::Regex;
use std::sync::OnceLock;
impl RegistryClient {
    pub(super) async fn request(&self, url: &Url, scope: &str) -> Result<Response> {
        if url.origin() != self.origin.origin() {
            return Err(Error::External("拒绝向其他地址发送仓库凭据".into()));
        }
        let cached = self
            .tokens
            .lock()
            .map_err(|_| Error::Internal("仓库认证状态不可用"))?
            .get(scope)
            .cloned();
        let request = self
            .client
            .get(url.clone())
            .header("Accept", MANIFEST_ACCEPT);
        let request = if let Some(token) = cached {
            request.bearer_auth(token)
        } else {
            request.basic_auth(&self.credentials.username, Some(&self.credentials.password))
        };
        let response = request
            .send()
            .await
            .map_err(|_| Error::External("无法连接目标仓库，请检查地址和网络".into()))?;
        if response.status() != 401 {
            return Ok(response);
        }
        let Some(challenge) = response
            .headers()
            .get_all("www-authenticate")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| {
                v.get(..7)
                    .is_some_and(|v| v.eq_ignore_ascii_case("bearer "))
            })
        else {
            return Ok(response);
        };
        let params = challenge_params(&challenge[7..])?;
        let realm = params
            .get("realm")
            .ok_or_else(|| Error::External("仓库认证地址缺失".into()))?;
        let mut realm =
            Url::parse(realm).map_err(|_| Error::External("仓库认证地址无效".into()))?;
        if !self.trusted_realm(&realm) {
            return Err(Error::External(
                "仓库返回不可信的认证地址，已阻止凭据发送".into(),
            ));
        }
        let service = params.get("service").map(String::as_str).unwrap_or("");
        realm.set_query(None);
        realm
            .query_pairs_mut()
            .append_pair("service", service)
            .append_pair("scope", scope)
            .append_pair("account", &self.credentials.username);
        let response = self
            .client
            .get(realm)
            .basic_auth(&self.credentials.username, Some(&self.credentials.password))
            .send()
            .await
            .map_err(|_| Error::External("仓库认证服务连接失败".into()))?;
        // Catalog permission is separate from repository pull permission.
        if scope == "registry:catalog:*" && matches!(response.status().as_u16(), 401 | 403) {
            return Ok(response);
        }
        let json: Value = serde_json::from_slice(&body(check(response)?, 64 * 1024).await?)
            .map_err(|_| Error::External("仓库认证响应格式不正确".into()))?;
        let token = json
            .get("token")
            .or_else(|| json.get("access_token"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty() && v.len() <= 16 * 1024)
            .ok_or_else(|| Error::External("仓库认证未返回有效令牌".into()))?;
        self.tokens
            .lock()
            .map_err(|_| Error::Internal("仓库认证状态不可用"))?
            .insert(scope.into(), token.into());
        self.client
            .get(url.clone())
            .header("Accept", MANIFEST_ACCEPT)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::External("仓库认证后查询失败".into()))
    }
    fn trusted_realm(&self, url: &Url) -> bool {
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return false;
        }
        if url.origin() == self.origin.origin() {
            return true;
        }
        // Legacy Aliyun ACR authenticates on a separate, region-specific official host.
        let host = self.origin.host_str().unwrap_or("");
        if self.origin.scheme() != "https"
            || url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !host.ends_with(".aliyuncs.com")
        {
            return false;
        }
        host.split('.').nth(1).is_some_and(|region| {
            url.host_str() == Some(format!("dockerauth.{region}.aliyuncs.com").as_str())
                && url.path() == "/auth"
        })
    }
}
fn challenge_params(challenge: &str) -> Result<HashMap<String, String>> {
    static PARAMS: OnceLock<Regex> = OnceLock::new();
    let regex =
        PARAMS.get_or_init(|| Regex::new(r#"(?:^|,)\s*([A-Za-z_]+)\s*=\s*"([^"\r\n]*)""#).unwrap());
    let mut values = HashMap::new();
    for capture in regex.captures_iter(challenge) {
        if values
            .insert(capture[1].to_ascii_lowercase(), capture[2].into())
            .is_some()
        {
            return Err(Error::External("仓库认证参数重复".into()));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod realm_tests {
    use super::*;
    #[test]
    fn aliyun_official_realm_works_for_both_legacy_and_personal_instance_endpoints() {
        for endpoint in [
            "registry.cn-hangzhou.aliyuncs.com",
            "crpi-demo123.cn-hangzhou.personal.cr.aliyuncs.com",
        ] {
            let client =
                RegistryClient::new(endpoint, Credentials::new("user".into(), "password".into()))
                    .unwrap();
            assert!(
                client.trusted_realm(
                    &Url::parse("https://dockerauth.cn-hangzhou.aliyuncs.com/auth").unwrap()
                ),
                "{endpoint}"
            );
            for hostile in [
                "http://dockerauth.cn-hangzhou.aliyuncs.com/auth",
                "https://dockerauth.cn-shanghai.aliyuncs.com/auth",
                "https://bucket.oss-cn-hangzhou.aliyuncs.com/auth",
                "https://dockerauth.cn-hangzhou.aliyuncs.com.attacker.test/auth",
            ] {
                assert!(
                    !client.trusted_realm(&Url::parse(hostile).unwrap()),
                    "{hostile}"
                );
            }
        }
    }
}

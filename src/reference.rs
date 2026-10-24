use crate::error::{Error, Result};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}
fn repo_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*$",
        )
        .unwrap()
    })
}
pub fn valid_repository(s: &str) -> bool {
    s.len() <= 255 && repo_regex().is_match(s)
}
pub fn valid_tag(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphanumeric() || b == b'_' || (i > 0 && (b == b'.' || b == b'-'))
        })
}
pub fn valid_digest(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}
pub fn normalize_endpoint(s: &str) -> Result<String> {
    if s.is_empty()
        || s.len() > 255
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
        || s.ends_with(':')
        || s.contains('/')
        || s.contains('@')
        || s.contains('?')
        || s.contains('#')
    {
        return Err(Error::invalid("仓库地址应为域名及可选端口，不含协议或路径"));
    }
    let url =
        url::Url::parse(&format!("https://{s}")).map_err(|_| Error::invalid("仓库地址无效"))?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::invalid("仓库地址无效"));
    }
    let host = url
        .host_str()
        .unwrap()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    if url.port() == Some(0) {
        return Err(Error::invalid("仓库端口不能为零"));
    }
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}
impl ImageReference {
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty()
            || input.len() > 1024
            || input.chars().any(|c| c.is_whitespace() || c.is_control())
            || input.contains("://")
            || input.starts_with('-')
        {
            return Err(Error::invalid("镜像引用格式不正确"));
        }
        let (name, digest) = if let Some((name, digest)) = input.rsplit_once('@') {
            if !valid_digest(digest) || name.contains('@') {
                return Err(Error::invalid("仅支持有效 sha256 digest"));
            }
            (name, Some(digest.to_owned()))
        } else {
            (input, None)
        };
        let slash = name.rfind('/');
        let (name, tag) = match name.rfind(':').filter(|i| slash.is_none_or(|s| *i > s)) {
            Some(i) => {
                let tag = &name[i + 1..];
                if !valid_tag(tag) {
                    return Err(Error::invalid("镜像标签无效"));
                }
                (&name[..i], Some(tag.to_owned()))
            }
            None => (name, None),
        };
        let first = name.split('/').next().unwrap_or("");
        let explicit = name.contains('/')
            && (first.contains('.') || first.contains(':') || first == "localhost");
        let (registry, repository) = if explicit {
            let (_, repo) = name.split_once('/').unwrap();
            (normalize_endpoint(first)?, repo.to_owned())
        } else {
            ("docker.io".into(), name.to_owned())
        };
        if !valid_repository(&repository) {
            return Err(Error::invalid("镜像仓库名称无效"));
        }
        let repository = if registry == "docker.io" && !repository.contains('/') {
            format!("library/{repository}")
        } else {
            repository
        };
        let tag = if digest.is_none() {
            Some(tag.unwrap_or_else(|| "latest".into()))
        } else {
            tag
        };
        Ok(Self {
            registry,
            repository,
            tag,
            digest,
        })
    }
    pub fn canonical(&self) -> String {
        let mut value = format!("{}/{}", self.registry, self.repository);
        if let Some(tag) = &self.tag {
            value.push(':');
            value.push_str(tag);
        }
        if let Some(digest) = &self.digest {
            value.push('@');
            value.push_str(digest);
        }
        value
    }
    pub fn with_digest(&self, digest: &str) -> Result<String> {
        if !valid_digest(digest) {
            return Err(Error::invalid("镜像 digest 无效"));
        }
        Ok(format!("{}/{}@{}", self.registry, self.repository, digest))
    }
}

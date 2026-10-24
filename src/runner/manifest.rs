use super::ImageTransport;
use crate::{
    domain::{Credentials, Platform},
    error::{Error, Result},
    reference::ImageReference,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub fn digest(raw: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(raw)))
}
pub struct ResolvedSource {
    pub index_digest: String,
    pub copy_digest: String,
    pub copy_reference: String,
}
pub fn resolve_source(
    transport: &impl ImageTransport,
    source: &str,
    platform: Platform,
    credentials: Option<&Credentials>,
) -> Result<ResolvedSource> {
    let source = ImageReference::parse(source)?;
    let raw = transport.raw_manifest(&source.canonical(), credentials)?;
    let root = digest(&raw);
    if source.digest.as_ref().is_some_and(|d| d != &root) {
        return Err(Error::invalid("源 manifest digest 校验失败"));
    }
    if platform == Platform::All {
        serde_json::from_slice::<Value>(&raw)?;
        return Ok(ResolvedSource {
            copy_reference: source.with_digest(&root)?,
            index_digest: root.clone(),
            copy_digest: root,
        });
    }
    let mut stack = vec![(raw, root.clone(), 0usize)];
    let mut visited = HashSet::new();
    let mut scheduled = HashSet::from([root.clone()]);
    let mut candidates = HashSet::new();
    while let Some((raw, current, depth)) = stack.pop() {
        if depth > 4 || visited.len() >= 64 {
            return Err(Error::invalid("镜像索引过深或条目过多"));
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        let manifest: Value = serde_json::from_slice(&raw)?;
        if let Some(entries) = manifest.get("manifests").and_then(Value::as_array) {
            for entry in entries {
                let nested = entry
                    .get("mediaType")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains("image.index") || s.contains("manifest.list"));
                let p = &entry["platform"];
                let matches = p["os"] == "linux"
                    && p["architecture"] == platform.arch().unwrap()
                    && (platform != Platform::LinuxArm64
                        || p["variant"].is_null()
                        || p["variant"] == "v8");
                if !matches && !(nested && p.is_null()) {
                    continue;
                }
                let child = entry["digest"]
                    .as_str()
                    .ok_or_else(|| Error::invalid("索引条目缺少 digest"))?;
                if scheduled.contains(child) {
                    continue;
                }
                if depth >= 4 || scheduled.len() >= 64 {
                    return Err(Error::invalid("镜像索引过深或条目过多"));
                }
                scheduled.insert(child.to_owned());
                let reference = source.with_digest(child)?;
                let bytes = transport.raw_manifest(&reference, credentials)?;
                if digest(&bytes) != child {
                    return Err(Error::invalid("子 manifest digest 校验失败"));
                }
                stack.push((bytes, child.into(), depth + 1));
            }
        } else {
            let config = transport.image_config(&source.with_digest(&current)?, credentials)?;
            let variant = &config["variant"];
            if config["os"] == "linux"
                && config["architecture"] == platform.arch().unwrap()
                && (platform != Platform::LinuxArm64 || variant.is_null() || variant == "v8")
            {
                candidates.insert(current);
            }
        }
    }
    if candidates.len() != 1 {
        return Err(Error::invalid(if candidates.is_empty() {
            format!("源镜像不支持 {}", platform.label())
        } else {
            format!("{} 存在多个不同候选镜像，无法唯一选择", platform.label())
        }));
    }
    let selected = candidates.into_iter().next().unwrap();
    Ok(ResolvedSource {
        index_digest: root,
        copy_reference: source.with_digest(&selected)?,
        copy_digest: selected,
    })
}

mod manifest;
mod skopeo;
use crate::{
    domain::*,
    error::{Error, Result},
};
pub use manifest::{ResolvedSource, digest, resolve_source};
use serde_json::Value;
pub use skopeo::SkopeoTransport;
use std::time::Instant;

pub trait ImageTransport {
    fn raw_manifest(&self, reference: &str, credentials: Option<&Credentials>) -> Result<Vec<u8>>;
    fn image_config(&self, reference: &str, credentials: Option<&Credentials>) -> Result<Value>;
    fn copy(
        &self,
        source: &str,
        target: &str,
        platform: Platform,
        source_credentials: Option<&Credentials>,
        target_credentials: &Credentials,
    ) -> Result<()>;
}
pub use crate::redact::{payload_secrets, redact};
pub fn run_batch(
    payload: TaskPayload,
    transport: &impl ImageTransport,
    attempt: u32,
) -> BatchResult {
    let secrets = payload_secrets(&payload);
    let mut items = Vec::new();
    for item in &payload.items {
        let start = Instant::now();
        let mut result = ItemResult {
            item_id: item.item_id,
            source: item.source.clone(),
            target: item.target.clone(),
            platform: item.platform,
            resolved_platform: None,
            source_index_digest: None,
            source_digest: None,
            target_digest: None,
            outcome: ItemOutcome::Failed,
            error: None,
            elapsed_ms: 0,
        };
        let operation = (|| -> Result<()> {
            let source = resolve_source(
                transport,
                &item.source,
                item.platform,
                item.source_credentials.as_ref(),
            )?;
            result.source_index_digest = Some(source.index_digest.clone());
            result.source_digest = Some(source.copy_digest.clone());
            result.resolved_platform = Some(item.platform.label().into());
            let old = match transport.raw_manifest(&item.target, Some(&item.target_credentials)) {
                Ok(raw) => Some(digest(&raw)),
                Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if old.as_deref() == Some(&source.copy_digest) {
                result.target_digest = old;
                result.outcome = ItemOutcome::Unchanged;
                return Ok(());
            }
            transport.copy(
                &source.copy_reference,
                &item.target,
                item.platform,
                item.source_credentials.as_ref(),
                &item.target_credentials,
            )?;
            let actual =
                digest(&transport.raw_manifest(&item.target, Some(&item.target_credentials))?);
            if actual != source.copy_digest {
                return Err(Error::External("复制后目标 digest 与源不一致".into()));
            }
            result.target_digest = Some(actual);
            result.outcome = ItemOutcome::Synced;
            Ok(())
        })();
        if let Err(error) = operation {
            result.error = Some(redact(error.to_string(), &secrets));
        }
        result.elapsed_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        items.push(result);
    }
    let summary = Summary {
        synced: items
            .iter()
            .filter(|i| i.outcome == ItemOutcome::Synced)
            .count(),
        unchanged: items
            .iter()
            .filter(|i| i.outcome == ItemOutcome::Unchanged)
            .count(),
        failed: items
            .iter()
            .filter(|i| i.outcome == ItemOutcome::Failed)
            .count(),
    };
    BatchResult {
        version: PROTOCOL_VERSION,
        task_id: payload.task_id,
        run_attempt: attempt,
        items,
        summary,
    }
}

use crate::{
    error::{Error, Result},
    reference::ImageReference,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKFLOW: &str = "web-sync.yml";
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u64,
    pub page_size: u32,
    pub pages: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Platform {
    #[default]
    #[serde(rename = "all")]
    All,
    #[serde(rename = "linux/amd64")]
    LinuxAmd64,
    #[serde(rename = "linux/arm64")]
    LinuxArm64,
}
impl Platform {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::LinuxAmd64 => "linux/amd64",
            Self::LinuxArm64 => "linux/arm64",
        }
    }
    pub fn arch(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::LinuxAmd64 => Some("amd64"),
            Self::LinuxArm64 => Some("arm64"),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}
impl Credentials {
    pub fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncItem {
    pub item_id: Uuid,
    pub source: String,
    pub target: String,
    pub platform: Platform,
    pub source_credentials: Option<Credentials>,
    pub target_credentials: Credentials,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPayload {
    pub version: u32,
    pub task_id: Uuid,
    pub items: Vec<SyncItem>,
}
impl TaskPayload {
    pub fn validate(&self) -> Result<()> {
        if self.version != PROTOCOL_VERSION {
            return Err(Error::invalid("任务协议版本不匹配"));
        }
        if self.items.is_empty() || self.items.len() > 20 {
            return Err(Error::invalid("每批需包含 1 至 20 条规则"));
        }
        let mut targets = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for item in &self.items {
            ImageReference::parse(&item.source)?;
            let target = ImageReference::parse(&item.target)?;
            if target.digest.is_some()
                || !targets.insert(target.canonical())
                || !ids.insert(item.item_id)
            {
                return Err(Error::invalid("目标引用或条目存在冲突"));
            }
            for c in std::iter::once(&item.target_credentials).chain(item.source_credentials.iter())
            {
                if c.username.len() > 1024
                    || c.password.len() > 4096
                    || c.username.contains(['\r', '\n'])
                    || c.password.contains(['\r', '\n'])
                {
                    return Err(Error::invalid("仓库凭据格式不正确"));
                }
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Dispatching,
    DispatchUnknown,
    DispatchFailed,
    Queued,
    Running,
    ResultPending,
    Succeeded,
    Failed,
    Cancelled,
    NeedsReview,
}
impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::DispatchFailed | Self::Succeeded | Self::Failed | Self::Cancelled
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatching => "dispatching",
            Self::DispatchUnknown => "dispatch_unknown",
            Self::DispatchFailed => "dispatch_failed",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::ResultPending => "result_pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::NeedsReview => "needs_review",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryInput {
    pub name: String,
    pub provider: String,
    pub endpoint: String,
    pub namespace: String,
    pub username: String,
    pub password: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistryView {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub endpoint: String,
    pub namespace: String,
    pub username: String,
    pub has_password: bool,
    pub enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleInput {
    pub name: String,
    pub source: String,
    pub registry_id: Uuid,
    pub target_repository: String,
    pub target_tag: String,
    #[serde(default)]
    pub platform: Platform,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub source_username: Option<String>,
    pub source_password: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleView {
    pub id: Uuid,
    pub name: String,
    pub source: String,
    pub registry_id: Uuid,
    pub target_repository: String,
    pub target_tag: String,
    pub platform: Platform,
    pub enabled: bool,
    pub source_username: Option<String>,
    pub has_source_password: bool,
    pub target: String,
    #[serde(default)]
    pub sync: RuleSyncStatus,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleSyncStatus {
    #[serde(default)]
    pub warning: Option<String>,
    pub status: String,
    pub task_id: Option<Uuid>,
    pub updated_at: Option<i64>,
    pub elapsed_ms: Option<u64>,
    pub error: Option<String>,
}
impl Default for RuleSyncStatus {
    fn default() -> Self {
        Self {
            warning: None,
            status: "never".into(),
            task_id: None,
            updated_at: None,
            elapsed_ms: None,
            error: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsInput {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub token: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsView {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub workflow: String,
    pub has_token: bool,
    #[serde(default)]
    pub ready: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub workflow: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(default)]
    pub refresh_error: Option<String>,
    pub id: Uuid,
    pub status: TaskStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub run_id: Option<u64>,
    pub run_attempt: u32,
    pub run_url: Option<String>,
    pub error: Option<String>,
    pub items: Vec<ItemView>,
    pub retry_of: Option<Uuid>,
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemView {
    pub item_id: Uuid,
    pub source: String,
    pub target: String,
    pub platform: Platform,
}
#[derive(Clone, Debug)]
pub struct TaskRecord {
    pub id: Uuid,
    pub status: TaskStatus,
    pub connection: ConnectionSnapshot,
    pub created_at: i64,
    pub run_id: Option<u64>,
    pub run_attempt: u32,
    pub next_poll_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemResult {
    pub item_id: Uuid,
    pub source: String,
    pub target: String,
    pub platform: Platform,
    pub resolved_platform: Option<String>,
    pub source_index_digest: Option<String>,
    pub source_digest: Option<String>,
    pub target_digest: Option<String>,
    pub outcome: ItemOutcome,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemOutcome {
    Synced,
    Unchanged,
    Failed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub synced: usize,
    pub unchanged: usize,
    pub failed: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchResult {
    pub version: u32,
    pub task_id: Uuid,
    pub run_attempt: u32,
    pub items: Vec<ItemResult>,
    pub summary: Summary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryView {
    pub checked_at: i64,
    pub scope: String,
    pub complete: bool,
    pub repositories: Vec<String>,
    pub warnings: Vec<String>,
    pub images: Vec<RemoteImage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteImage {
    pub repository: String,
    pub tag: String,
    pub reference: String,
    pub platforms: Vec<String>,
    pub digest: Option<String>,
    pub error: Option<String>,
}

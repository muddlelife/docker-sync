use super::*;
use crate::{
    domain::*,
    reference::{ImageReference, normalize_endpoint, valid_repository, valid_tag},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub(crate) struct RegistryRecord {
    pub view: RegistryView,
    pub password: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct RuleRecord {
    pub view: RuleView,
    pub source_password: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct SettingsRecord {
    view: SettingsView,
    token: String,
    #[serde(default)]
    revision: String,
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn target(registry: &RegistryView, repository: &str, tag: &str) -> String {
    format!(
        "{}/{}/{}:{}",
        registry.endpoint, registry.namespace, repository, tag
    )
}
fn list<T: DeserializeOwned>(conn: &Connection, table: &str) -> Result<Vec<T>> {
    let mut statement = conn.prepare(&format!("SELECT data FROM {table} ORDER BY rowid DESC"))?;
    let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|row| decode(row?)).collect()
}
impl Store {
    pub fn registry_connection(
        &self,
        id: Uuid,
    ) -> Result<(RegistryView, Credentials, Vec<String>)> {
        let conn = self.conn()?;
        let record = read::<RegistryRecord>(&conn, "registries", &id.to_string())?;
        let credentials = Credentials::new(
            record.view.username.clone(),
            self.crypto.open_secret(&record.password)?,
        );
        let known = list::<RuleRecord>(&conn, "rules")?
            .into_iter()
            .filter(|r| r.view.registry_id == id)
            .map(|r| r.view.target_repository)
            .collect();
        Ok((record.view, credentials, known))
    }
    pub fn registries(&self) -> Result<Vec<RegistryView>> {
        Ok(list::<RegistryRecord>(&self.conn()?, "registries")?
            .into_iter()
            .map(|r| r.view)
            .collect())
    }
    pub fn rules(&self) -> Result<Vec<RuleView>> {
        let conn = self.conn()?;
        list::<RuleRecord>(&conn, "rules")?
            .into_iter()
            .map(|r| {
                let mut view = r.view;
                view.sync = Self::rule_sync(&conn, &view)?;
                Ok(view)
            })
            .collect()
    }
    pub fn settings(&self) -> Result<Option<SettingsView>> {
        let conn = self.conn()?;
        let value: Option<String> = conn
            .query_row("SELECT data FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()?;
        value
            .map(|v| decode::<SettingsRecord>(v).map(|r| r.view))
            .transpose()
    }
    pub fn token(&self) -> Result<String> {
        let conn = self.conn()?;
        let value: String = conn
            .query_row("SELECT data FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::invalid("请先配置 GitHub 连接"))?;
        self.crypto
            .open_secret(&decode::<SettingsRecord>(value)?.token)
    }
    pub fn connection_setup(&self) -> Result<(SettingsView, String, String)> {
        let value: String = self
            .conn()?
            .query_row("SELECT data FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::invalid("请先保存 GitHub 连接设置"))?;
        let record: SettingsRecord = decode(value)?;
        Ok((
            record.view,
            record.revision,
            self.crypto.open_secret(&record.token)?,
        ))
    }
    pub fn mark_settings_ready(&self, revision: &str) -> Result<()> {
        self.set_settings_ready(revision, true)
    }
    pub fn mark_settings_unready(&self, revision: &str) -> Result<()> {
        self.set_settings_ready(revision, false)
    }
    fn set_settings_ready(&self, revision: &str, ready: bool) -> Result<()> {
        self.tx(|tx| {
            let value: String =
                tx.query_row("SELECT data FROM settings WHERE id=1", [], |r| r.get(0))?;
            let mut record: SettingsRecord = decode(value)?;
            if record.revision != revision {
                return Err(Error::Conflict("连接设置已变更，请重新初始化".into()));
            }
            record.view.ready = ready;
            tx.execute("UPDATE settings SET data=?1 WHERE id=1", [json(&record)?])?;
            Ok(())
        })
    }
    pub fn save_settings(&self, input: SettingsInput) -> Result<SettingsView> {
        let valid_part = |s: &str| {
            text(s, 100)
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        };
        if !valid_part(&input.owner) || !valid_part(&input.repo) || !text(&input.branch, 255) {
            return Err(Error::invalid("GitHub 仓库或分支无效"));
        }
        self.tx(|tx| {
            let old:Option<String>=tx.query_row("SELECT data FROM settings WHERE id=1",[],|r|r.get(0)).optional()?;
            let old=old.map(decode::<SettingsRecord>).transpose()?;
            let active:i64=tx.query_row("SELECT count(*) FROM tasks WHERE status NOT IN ('succeeded','failed','cancelled','dispatch_failed')",[],|r|r.get(0))?;
            if active>0 && old.as_ref().is_some_and(|o|o.view.owner!=input.owner || o.view.repo!=input.repo || o.view.branch!=input.branch) {
                return Err(Error::Conflict("有未完成任务，暂时不能更换 GitHub 仓库或分支".into()));
            }
            let token=match input.token.as_deref().filter(|s|!s.is_empty()) {
                Some(value) if text(value,4096) => self.crypto.seal_secret(value)?,
                Some(_) => return Err(Error::invalid("Token 格式不正确")),
                None => old.map(|o|o.token).ok_or_else(||Error::invalid("首次配置需要 GitHub Token"))?,
            };
            let view=SettingsView{owner:input.owner.clone(),repo:input.repo.clone(),branch:input.branch.clone(),workflow:WORKFLOW.into(),has_token:true,ready:false};
            tx.execute("INSERT INTO settings(id,data) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data",[json(&SettingsRecord{view:view.clone(),token,revision:Uuid::new_v4().to_string()})?])?;
            Ok(view)
        })
    }
    pub fn save_registry(&self, id: Option<Uuid>, input: RegistryInput) -> Result<RegistryView> {
        if !text(&input.name, 100)
            || !text(&input.username, 1024)
            || !valid_repository(&input.namespace)
            || input.namespace.contains('/')
            || input.provider != "aliyun_acr"
        {
            return Err(Error::invalid("请填写有效的 ACR 名称、命名空间和用户名"));
        }
        let endpoint = normalize_endpoint(&input.endpoint)?;
        self.tx(|tx| {
            let old=if let Some(id)=id {Some(read::<RegistryRecord>(tx,"registries",&id.to_string())?)} else {None};
            let password=match input.password.as_deref().filter(|p|!p.is_empty()) {
                Some(p) if text(p,4096) => self.crypto.seal_secret(p)?,
                Some(_) => return Err(Error::invalid("密码格式不正确")),
                None => old.map(|r|r.password).ok_or_else(||Error::invalid("首次配置需要仓库密码"))?,
            };
            let view=RegistryView{id:id.unwrap_or_else(Uuid::new_v4),name:input.name.clone(),provider:input.provider.clone(),endpoint:endpoint.clone(),namespace:input.namespace.clone(),username:input.username.clone(),has_password:true,enabled:input.enabled};
            let record=RegistryRecord{view:view.clone(),password};
            tx.execute("INSERT INTO registries(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",rusqlite::params![view.id.to_string(),json(&record)?])?;
            for mut rule in list::<RuleRecord>(tx,"rules")? {
                if rule.view.registry_id!=view.id {continue;}
                rule.view.target=target(&view,&rule.view.target_repository,&rule.view.target_tag);
                let conflicts:i64=tx.query_row("SELECT count(*) FROM rules WHERE enabled=1 AND target=?1 AND id!=?2",rusqlite::params![rule.view.target,rule.view.id.to_string()],|r|r.get(0))?;
                if rule.view.enabled && conflicts>0 {return Err(Error::Conflict("修改后存在重复目标镜像".into()));}
                tx.execute("UPDATE rules SET data=?1,target=?2 WHERE id=?3",rusqlite::params![json(&rule)?,rule.view.target,rule.view.id.to_string()])?;
            }
            Ok(view)
        })
    }
    pub fn delete_registry(&self, id: Uuid) -> Result<()> {
        self.tx(|tx| {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM rules WHERE registry_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            if count > 0 {
                return Err(Error::Conflict("仓库仍被同步规则引用，请先移除规则".into()));
            }
            if tx.execute("DELETE FROM registries WHERE id=?1", [id.to_string()])? == 0 {
                return Err(Error::NotFound("仓库不存在".into()));
            }
            Ok(())
        })
    }
    pub fn save_rule(&self, id: Option<Uuid>, input: RuleInput) -> Result<RuleView> {
        if !text(&input.name, 100)
            || !valid_repository(&input.target_repository)
            || !valid_tag(&input.target_tag)
        {
            return Err(Error::invalid("规则名称、目标仓库名称或标签无效"));
        }
        let source = ImageReference::parse(&input.source)?.canonical();
        self.tx(|tx| {
            let registry=read::<RegistryRecord>(tx,"registries",&input.registry_id.to_string())?;
            if input.enabled && !registry.view.enabled {return Err(Error::invalid("目标仓库已停用"));}
            let old=if let Some(id)=id {Some(read::<RuleRecord>(tx,"rules",&id.to_string())?)} else {None};
            let password=match input.source_password.as_deref().filter(|p|!p.is_empty()) {
                Some(p) if text(p,4096) => Some(self.crypto.seal_secret(p)?),
                Some(_) => return Err(Error::invalid("源仓库密码无效")),
                None => old.and_then(|r|r.source_password),
            };
            let source_username=input.source_username.clone().filter(|s|!s.is_empty());
            if source_username.as_ref().is_some_and(|s|!text(s,1024)) || (source_username.is_some()!=password.is_some()) {
                return Err(Error::invalid("私有源仓库需要同时提供用户名和密码"));
            }
            let view=RuleView{id:id.unwrap_or_else(Uuid::new_v4),name:input.name.clone(),source:source.clone(),registry_id:input.registry_id,target_repository:input.target_repository.clone(),target_tag:input.target_tag.clone(),platform:input.platform,enabled:input.enabled,source_username,has_source_password:password.is_some(),target:target(&registry.view,&input.target_repository,&input.target_tag),sync:RuleSyncStatus::default()};
            let conflicts:i64=tx.query_row("SELECT count(*) FROM rules WHERE enabled=1 AND target=?1 AND id!=?2",rusqlite::params![view.target,view.id.to_string()],|r|r.get(0))?;
            if view.enabled && conflicts>0 {return Err(Error::Conflict("该目标标签已有启用规则，请使用独立标签".into()));}
            tx.execute("INSERT INTO rules(id,registry_id,data,target,enabled) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET registry_id=excluded.registry_id,data=excluded.data,target=excluded.target,enabled=excluded.enabled",
                rusqlite::params![view.id.to_string(),view.registry_id.to_string(),json(&RuleRecord{view:view.clone(),source_password:password})?,view.target,view.enabled])?;
            Ok(view)
        })
    }
    pub fn delete_rule(&self, id: Uuid) -> Result<()> {
        if self
            .conn()?
            .execute("DELETE FROM rules WHERE id=?1", [id.to_string()])?
            == 0
        {
            return Err(Error::NotFound("规则不存在".into()));
        }
        Ok(())
    }
}

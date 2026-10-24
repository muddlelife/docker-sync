use crate::{
    crypto::Crypto,
    error::{Error, Result},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

// 只由本地运维命令读取；不实现 Debug，避免把初始化凭据写进日志。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    version: u32,
    pub admin_password: String,
    pub app_encryption_key: String,
    pub sync_payload_key: String,
}
impl Bootstrap {
    fn random_password() -> String {
        let mut value = [0; 24];
        rand::rngs::OsRng.fill_bytes(&mut value);
        URL_SAFE_NO_PAD.encode(value)
    }
    fn random_key() -> String {
        let mut value = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut value);
        hex::encode(value)
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.admin_password.len() < 12 || self.admin_password.len() > 1024 {
            return Err(Error::invalid("初始化凭据无效，请恢复完整数据备份"));
        }
        let app = Crypto::key_from_hex(&self.app_encryption_key)?;
        let task = Crypto::key_from_hex(&self.sync_payload_key)?;
        if app == task {
            return Err(Error::invalid("两把加密密钥必须不同"));
        }
        Ok(())
    }
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("bootstrap.json");
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(Error::invalid("初始化凭据必须是普通文件"));
        }
        let mut bytes = Vec::new();
        File::open(&path)?.take(16385).read_to_end(&mut bytes)?;
        if bytes.len() > 16384 {
            return Err(Error::invalid("初始化凭据文件过大"));
        }
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::invalid("初始化凭据文件损坏，请恢复备份"))?;
        value.validate()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(value)
    }
    fn temporary(&self, dir: &Path) -> Result<tempfile::NamedTempFile> {
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        Ok(file)
    }
    pub fn load_or_create(dir: &Path) -> Result<(Self, bool)> {
        let fresh = !dir.exists();
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        if fresh {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.join("bootstrap.json");
        if path.exists() {
            return Ok((Self::load(dir)?, false));
        }
        if dir.join("docker-sync.db").exists() {
            return Err(Error::invalid(
                "数据库存在但初始化凭据丢失，请恢复完整数据备份",
            ));
        }
        let value = Self {
            version: 1,
            admin_password: Self::random_password(),
            app_encryption_key: Self::random_key(),
            sync_payload_key: Self::random_key(),
        };
        match value.temporary(dir)?.persist_noclobber(&path) {
            Ok(_) => {
                File::open(dir)?.sync_all()?;
                Ok((value, true))
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok((Self::load(dir)?, false))
            }
            Err(_) => Err(Error::Internal("无法持久化初始化凭据")),
        }
    }
    pub fn reset_password(dir: &Path) -> Result<Self> {
        let (mut value, _) = Self::load_or_create(dir)?;
        value.admin_password = Self::random_password();
        value
            .temporary(dir)?
            .persist(dir.join("bootstrap.json"))
            .map_err(|_| Error::Internal("无法保存新密码"))?;
        File::open(dir)?.sync_all()?;
        Ok(value)
    }
    pub fn change_password(dir: &Path, password: String) -> Result<Self> {
        let mut value = Self::load(dir)?;
        value.admin_password = password;
        value.validate()?;
        value
            .temporary(dir)?
            .persist(dir.join("bootstrap.json"))
            .map_err(|_| Error::Internal("无法保存新密码"))?;
        File::open(dir)?.sync_all()?;
        Ok(value)
    }
}

use crate::{
    domain::TaskPayload,
    error::{Error, Result},
};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use uuid::Uuid;

#[derive(Clone)]
pub struct Crypto {
    data_key: [u8; 32],
    task_key: [u8; 32],
}
impl Crypto {
    #[cfg(feature = "web")]
    pub fn task_secret_name(&self) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "DOCKER_SYNC_KEY_{}",
            hex::encode(Sha256::digest(self.task_key))[..24].to_uppercase()
        )
    }
    #[cfg(feature = "web")]
    pub fn task_key_hex(&self) -> String {
        hex::encode(self.task_key)
    }
    pub fn new(data_key: [u8; 32], task_key: [u8; 32]) -> Self {
        Self { data_key, task_key }
    }
    pub fn key_from_hex(value: &str) -> Result<[u8; 32]> {
        let bytes =
            hex::decode(value).map_err(|_| Error::invalid("密钥必须是 64 位十六进制字符串"))?;
        bytes
            .try_into()
            .map_err(|_| Error::invalid("密钥必须是 32 字节"))
    }
    fn seal(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Result<String> {
        let mut nonce = [0; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let cipher = Aes256Gcm::new_from_slice(key).unwrap();
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| Error::Internal("加密失败"))?;
        let mut output = nonce.to_vec();
        output.extend(encrypted);
        Ok(URL_SAFE_NO_PAD.encode(output))
    }
    fn open(key: &[u8; 32], value: &str, aad: &[u8]) -> Result<Vec<u8>> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| Error::invalid("加密数据无效"))?;
        if bytes.len() < 28 {
            return Err(Error::invalid("加密数据无效"));
        }
        Aes256Gcm::new_from_slice(key)
            .unwrap()
            .decrypt(
                Nonce::from_slice(&bytes[..12]),
                Payload {
                    msg: &bytes[12..],
                    aad,
                },
            )
            .map_err(|_| Error::invalid("密钥不匹配或数据已被篡改"))
    }
    pub fn seal_secret(&self, value: &str) -> Result<String> {
        Self::seal(&self.data_key, value.as_bytes(), b"docker-sync:secret:v1")
    }
    pub fn open_secret(&self, value: &str) -> Result<String> {
        String::from_utf8(Self::open(&self.data_key, value, b"docker-sync:secret:v1")?)
            .map_err(|_| Error::Internal("凭据无法读取"))
    }
    pub fn seal_task(&self, payload: &TaskPayload) -> Result<String> {
        payload.validate()?;
        let value = Self::seal(
            &self.task_key,
            &serde_json::to_vec(payload)?,
            format!("docker-sync:task:v1:{}", payload.task_id).as_bytes(),
        )?;
        if value.len() + 256 > 48 * 1024 {
            return Err(Error::invalid("任务超过 48 KiB，请分批同步"));
        }
        Ok(value)
    }
    pub fn open_task(&self, id: Uuid, value: &str) -> Result<TaskPayload> {
        if value.len() + 256 > 48 * 1024 {
            return Err(Error::invalid("任务过大"));
        }
        let plain = Self::open(
            &self.task_key,
            value,
            format!("docker-sync:task:v1:{id}").as_bytes(),
        )?;
        let payload: TaskPayload = serde_json::from_slice(&plain)?;
        if payload.task_id != id {
            return Err(Error::invalid("任务标识不匹配"));
        }
        payload.validate()?;
        Ok(payload)
    }
}

use super::*;
use std::{
    collections::HashMap,
    io::{Cursor, Read},
};

impl GithubClient {
    pub(super) async fn download_archive(&self, path: &str) -> Result<HashMap<String, Vec<u8>>> {
        let response = self
            .request(Method::GET, path)
            .await?
            .send()
            .await
            .map_err(|_| Error::External("无法下载任务文件".into()))?;
        let response = if response.status() == 302 {
            let location = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or_else(|| Error::invalid("下载地址缺失"))?;
            let url = reqwest::Url::parse(location).map_err(|_| Error::invalid("下载地址无效"))?;
            let local_base = self.base.starts_with("http://127.0.0.1:")
                || self.base.starts_with("http://localhost:");
            if url.scheme() != "https"
                && !(local_base
                    && url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost")))
            {
                return Err(Error::invalid("下载必须使用 HTTPS"));
            }
            // 签名下载地址使用独立无认证请求，绝不转发 GitHub Token。
            self.download_client
                .get(url)
                .send()
                .await
                .map_err(|_| Error::External("下载存储文件失败".into()))?
        } else {
            response
        };
        if !response.status().is_success() {
            return Err(Error::External("任务文件不可用或已过期".into()));
        }
        let bytes = Self::bytes(response, 10 * 1024 * 1024).await?;
        rocket::tokio::task::spawn_blocking(move || read_archive(bytes))
            .await
            .map_err(|_| Error::Internal("读取压缩文件失败"))?
    }
}
pub fn read_archive(bytes: Vec<u8>) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| Error::invalid("下载文件不是有效 ZIP"))?;
    if archive.len() > 100 {
        return Err(Error::invalid("压缩包文件过多"));
    }
    let mut total = 0u64;
    let mut entries = HashMap::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| Error::invalid("压缩包无法读取"))?;
        if file.enclosed_name().is_none()
            || file.name().contains('\\')
            || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(Error::invalid("压缩包路径不安全"));
        }
        if file.is_dir() {
            continue;
        }
        total = total
            .checked_add(file.size())
            .ok_or_else(|| Error::invalid("压缩包过大"))?;
        if total > 20 * 1024 * 1024 {
            return Err(Error::invalid("压缩包解压后过大"));
        }
        let name = file.name().to_owned();
        let mut data = Vec::new();
        file.by_ref()
            .take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut data)?;
        if data.len() as u64 != file.size() || entries.insert(name, data).is_some() {
            return Err(Error::invalid("压缩包长度异常或存在重复文件"));
        }
    }
    Ok(entries)
}

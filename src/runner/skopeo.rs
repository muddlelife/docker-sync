use super::{ImageTransport, redact};
use crate::{
    domain::{Credentials, Platform},
    error::{Error, Result},
    reference::ImageReference,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub struct SkopeoTransport {
    pub executable: String,
    pub timeout: Duration,
}
impl Default for SkopeoTransport {
    fn default() -> Self {
        Self {
            executable: "skopeo".into(),
            timeout: Duration::from_secs(2400),
        }
    }
}
impl SkopeoTransport {
    fn auth(reference: &str, credentials: Option<&Credentials>) -> Result<tempfile::NamedTempFile> {
        let mut file = tempfile::NamedTempFile::new()?;
        let registry = ImageReference::parse(reference)?.registry;
        let auth = if let Some(c) = credentials {
            serde_json::json!({"auths":{registry:{"auth":STANDARD.encode(format!("{}:{}",c.username,c.password))}}})
        } else {
            serde_json::json!({"auths":{}})
        };
        file.write_all(&serde_json::to_vec(&auth)?)?;
        Ok(file)
    }
    fn invoke(&self, args: &[String], secrets: &[String]) -> Result<Vec<u8>> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| Error::External("无法启动 Skopeo，请检查安装".into()))?;
        let (sender, receiver) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        for (is_out, mut pipe) in [
            (true, Box::new(stdout) as Box<dyn Read + Send>),
            (false, Box::new(stderr) as Box<dyn Read + Send>),
        ] {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe
                    .by_ref()
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes);
                let _ = sender.send((is_out, result.map(|_| bytes)));
            });
        }
        drop(sender);
        let start = Instant::now();
        let mut out = None;
        let mut err = None;
        let mut status = None;
        while out.is_none() || err.is_none() || status.is_none() {
            while let Ok((is_out, result)) = receiver.try_recv() {
                let value = result.map_err(|_| Error::Internal("读取 Skopeo 输出失败"))?;
                if value.len() > 2 * 1024 * 1024 {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::External("Skopeo 输出超过限制".into()));
                }
                if is_out {
                    out = Some(value)
                } else {
                    err = Some(value)
                }
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::External("Skopeo 操作超时".into()));
            }
            if out.is_none() || err.is_none() || status.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if !status.unwrap().success() {
            let text = String::from_utf8_lossy(&err.unwrap()).into_owned();
            let lower = text.to_ascii_lowercase();
            if lower.contains("unauthorized")
                || lower.contains("denied")
                || lower.contains("authentication")
            {
                return Err(Error::External("镜像仓库身份验证失败或权限不足".into()));
            }
            if lower.contains("manifest unknown")
                || lower.contains("name unknown")
                || lower.contains("manifest_unknown")
                || lower.contains("name_unknown")
            {
                return Err(Error::NotFound("镜像不存在".into()));
            }
            return Err(Error::External(format!(
                "Skopeo 操作失败：{}",
                redact(text, secrets).chars().take(1500).collect::<String>()
            )));
        }
        Ok(out.unwrap())
    }
    fn inspect(&self, r: &str, credentials: Option<&Credentials>, mode: &str) -> Result<Vec<u8>> {
        let auth = Self::auth(r, credentials)?;
        let secrets = credentials
            .map(|c| {
                vec![
                    c.password.clone(),
                    STANDARD.encode(format!("{}:{}", c.username, c.password)),
                ]
            })
            .unwrap_or_default();
        self.invoke(
            &[
                "inspect".into(),
                mode.into(),
                "--authfile".into(),
                auth.path().to_string_lossy().into(),
                format!("docker://{r}"),
            ],
            &secrets,
        )
    }
}
impl ImageTransport for SkopeoTransport {
    fn raw_manifest(&self, r: &str, c: Option<&Credentials>) -> Result<Vec<u8>> {
        self.inspect(r, c, "--raw")
    }
    fn image_config(&self, r: &str, c: Option<&Credentials>) -> Result<serde_json::Value> {
        Ok(serde_json::from_slice(&self.inspect(r, c, "--config")?)?)
    }
    fn copy(
        &self,
        source: &str,
        target: &str,
        platform: Platform,
        src: Option<&Credentials>,
        dest: &Credentials,
    ) -> Result<()> {
        let source_auth = Self::auth(source, src)?;
        let target_auth = Self::auth(target, Some(dest))?;
        let mut args = Vec::new();
        if let Some(arch) = platform.arch() {
            args.extend([
                "--override-os".into(),
                "linux".into(),
                "--override-arch".into(),
                arch.into(),
            ]);
        }
        args.push("copy".into());
        if platform == Platform::All {
            args.push("--all".into());
        }
        args.extend([
            "--preserve-digests".into(),
            "--retry-times".into(),
            "3".into(),
            "--src-authfile".into(),
            source_auth.path().to_string_lossy().into(),
            "--dest-authfile".into(),
            target_auth.path().to_string_lossy().into(),
            format!("docker://{source}"),
            format!("docker://{target}"),
        ]);
        let mut secrets = vec![
            dest.password.clone(),
            STANDARD.encode(format!("{}:{}", dest.username, dest.password)),
        ];
        if let Some(c) = src {
            secrets.extend([
                c.password.clone(),
                STANDARD.encode(format!("{}:{}", c.username, c.password)),
            ]);
        }
        self.invoke(&args, &secrets)?;
        Ok(())
    }
}

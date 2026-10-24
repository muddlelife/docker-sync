use clap::{Parser, Subcommand};
use docker_sync::{
    crypto::Crypto,
    domain::*,
    error::{Error, Result},
    runner::{SkopeoTransport, payload_secrets, run_batch},
};
use std::{io::Write, path::PathBuf};
use uuid::Uuid;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Execute {
        #[arg(long, default_value = "artifacts")]
        output_dir: PathBuf,
    },
}
fn env(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| Error::invalid(format!("缺少环境变量 {name}")))
}
fn execute(payload: TaskPayload, dir: PathBuf) -> Result<bool> {
    std::fs::create_dir_all(&dir)?;
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        for secret in payload_secrets(&payload) {
            if !secret.is_empty() {
                println!(
                    "::add-mask::{}",
                    secret
                        .replace('%', "%25")
                        .replace('\r', "%0D")
                        .replace('\n', "%0A")
                );
            }
        }
    }
    let attempt = std::env::var("GITHUB_RUN_ATTEMPT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let result = run_batch(payload, &SkopeoTransport::default(), attempt);
    let mut logs = std::fs::File::create(dir.join("sync.log"))?;
    for item in &result.items {
        let line = format!(
            "{} → {} [{}]: {:?}{}\n",
            item.source,
            item.target,
            item.platform.label(),
            item.outcome,
            item.error
                .as_ref()
                .map(|e| format!(" - {e}"))
                .unwrap_or_default()
        );
        print!("{line}");
        logs.write_all(line.as_bytes())?;
    }
    std::fs::write(dir.join("result.json"), serde_json::to_vec_pretty(&result)?)?;
    Ok(result.summary.failed == 0)
}
fn run() -> Result<bool> {
    match Cli::parse().command {
        Action::Execute { output_dir } => {
            if env("TASK_PROTOCOL_VERSION")? != "1" {
                return Err(Error::invalid("工作流任务协议版本不匹配"));
            }
            let encoded_key = std::env::var("SYNC_PAYLOAD_KEY").unwrap_or_default();
            let key = Crypto::key_from_hex(&encoded_key).map_err(|_| Error::invalid(
                "SYNC_PAYLOAD_KEY 无效，应为 64 位十六进制任务密钥；请回到 Web 控制台重新初始化连接，由系统自动配置 GitHub Secret"
            ))?;
            let id =
                Uuid::parse_str(&env("TASK_ID")?).map_err(|_| Error::invalid("任务 ID 无效"))?;
            let crypto = Crypto::new([0; 32], key);
            execute(crypto.open_task(id, &env("TASK_PAYLOAD")?)?, output_dir)
        }
    }
}
fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}

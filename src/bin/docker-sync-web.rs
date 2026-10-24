use docker_sync::{
    bootstrap::Bootstrap,
    error::Error,
    github::GithubClient,
    store::Store,
    web::{AppConfig, build_rocket},
};
#[rocket::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "data".into()));
    let action = std::env::args().nth(1);
    match action.as_deref() {
        Some("show-setup") => {
            let (setup, _) = Bootstrap::load_or_create(&dir)?;
            println!("管理员密码：{}", setup.admin_password);
            return Ok(());
        }
        Some("reset-password") => {
            let setup = Bootstrap::reset_password(&dir)?;
            println!(
                "新管理员密码：{}\n请重启服务使新密码生效。",
                setup.admin_password
            );
            return Ok(());
        }
        Some("health") => {
            let port: u16 = std::env::var("ROCKET_PORT")
                .unwrap_or_else(|_| "8000".into())
                .parse()?;
            let response = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(3))
                .build()?
                .get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .await?;
            if !response.status().is_success() {
                return Err(Error::External("健康检查未通过".into()).into());
            }
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("docker-sync-web [show-setup|reset-password|health]\n不带参数启动 Web 服务。");
            return Ok(());
        }
        Some(_) => return Err(Error::invalid("未知命令；使用 --help 查看命令").into()),
        None => {}
    }
    let (config, crypto) = AppConfig::from_env()?;
    let store = Store::open(config.data_dir.join("docker-sync.db"), crypto)?;
    let github = GithubClient::for_store(store.clone())?;
    build_rocket(config, store, github).launch().await?;
    Ok(())
}

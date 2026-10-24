mod account;
mod auth;
mod config;
mod images;
mod rules;
mod tasks;
use crate::{
    crypto::Crypto,
    error::{Error, Result},
    github::GithubClient,
    store::Store,
    worker::{SystemClock, Worker},
};
use argon2::{
    Argon2, PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
pub use auth::{AdminSession, CsrfGuard};
use rocket::{
    Build, Request, Rocket, State,
    fairing::AdHoc,
    fs::FileServer,
    http::{ContentType, Status},
    response::{Responder, Response},
    serde::json::Json,
};
use rocket_dyn_templates::{Template, context};
use serde_json::json;
use std::{
    collections::HashMap,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct AppConfig {
    pub data_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub password_hash: String,
    pub secure_cookie: bool,
    pub worker_enabled: bool,
}
impl AppConfig {
    pub fn for_password(password: &str) -> Result<Self> {
        if password.len() < 12 || password.len() > 1024 {
            return Err(Error::invalid("管理员密码需为 12 至 1024 字节"));
        }
        let hash = hash_password(password)?;
        Ok(Self {
            data_dir: "data".into(),
            assets_dir: env!("CARGO_MANIFEST_DIR").into(),
            password_hash: hash,
            secure_cookie: true,
            worker_enabled: true,
        })
    }
    pub fn from_env() -> Result<(Self, Crypto)> {
        let dir: PathBuf = std::env::var("DATA_DIR")
            .unwrap_or_else(|_| "data".into())
            .into();
        let (bootstrap, created) = crate::bootstrap::Bootstrap::load_or_create(&dir)?;
        let mut config = Self::for_password(&bootstrap.admin_password)?;
        config.data_dir = dir;
        if let Ok(dir) = std::env::var("ASSET_DIR") {
            config.assets_dir = dir.into();
        }
        config.secure_cookie = match std::env::var("COOKIE_SECURE").as_deref() {
            Ok("true") => true,
            Ok("false") | Err(_) => false,
            _ => return Err(Error::invalid("COOKIE_SECURE 只能为 true 或 false")),
        };
        if created {
            eprintln!(
                "初始化完成。运行 docker compose exec web /app/docker-sync-web show-setup 获取登录信息。"
            );
        }
        Ok((
            config,
            Crypto::new(
                Crypto::key_from_hex(&bootstrap.app_encryption_key)?,
                Crypto::key_from_hex(&bootstrap.sync_payload_key)?,
            ),
        ))
    }
}

pub struct AppState {
    pub store: Store,
    pub config: AppConfig,
    pub github: GithubClient,
    pub worker: Worker,
    pub logins: Mutex<HashMap<String, Vec<i64>>>,
    pub inventory_gate: rocket::tokio::sync::Semaphore,
    pub(crate) password: Arc<Mutex<String>>,
}
pub(crate) fn hash_password(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| Error::Internal("密码哈希失败"))
}
pub(crate) async fn db<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    let store = state.store.clone();
    rocket::tokio::task::spawn_blocking(move || f(store))
        .await
        .map_err(|_| Error::Internal("数据库请求执行失败"))?
}
pub(crate) fn id(value: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(value).map_err(|_| Error::invalid("记录 ID 格式不正确"))
}
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(self, _: &'r Request<'_>) -> rocket::response::Result<'static> {
        let (status, code) = match &self {
            Error::Invalid(_) => (Status::UnprocessableEntity, "invalid"),
            Error::Conflict(_) => (Status::Conflict, "conflict"),
            Error::NotFound(_) => (Status::NotFound, "not_found"),
            Error::Unauthorized => (Status::Unauthorized, "unauthorized"),
            Error::RateLimited(_) => (Status::TooManyRequests, "rate_limited"),
            _ => (Status::InternalServerError, "error"),
        };
        let body = json!({"code":code,"message":self.to_string()}).to_string();
        Response::build()
            .status(status)
            .header(ContentType::JSON)
            .sized_body(body.len(), Cursor::new(body))
            .ok()
    }
}
#[rocket::catch(401)]
fn unauthorized(request: &Request<'_>) -> GuardResponse {
    if request.uri().path().as_str().starts_with("/api/") {
        let body = json!({"code":"unauthorized","message":"请先登录"}).to_string();
        GuardResponse(
            Response::build()
                .status(Status::Unauthorized)
                .header(ContentType::JSON)
                .sized_body(body.len(), Cursor::new(body))
                .finalize(),
        )
    } else {
        GuardResponse(
            Response::build()
                .status(Status::SeeOther)
                .raw_header("Location", "/login")
                .finalize(),
        )
    }
}
struct GuardResponse(Response<'static>);
impl<'r> Responder<'r, 'static> for GuardResponse {
    fn respond_to(self, _: &'r Request<'_>) -> rocket::response::Result<'static> {
        Ok(self.0)
    }
}
#[rocket::catch(403)]
fn forbidden() -> (Status, Json<serde_json::Value>) {
    (
        Status::Forbidden,
        Json(json!({"code":"csrf","message":"请求校验失败，请刷新页面后重试"})),
    )
}
#[rocket::get("/")]
fn dashboard(_admin: AdminSession) -> Template {
    Template::render("dashboard", context! {page:"dashboard",title:"概览"})
}
#[rocket::get("/settings")]
fn settings_page(_admin: AdminSession) -> Template {
    Template::render("settings", context! {page:"settings",title:"连接设置"})
}
#[rocket::get("/registries")]
fn registries_page(_admin: AdminSession) -> Template {
    Template::render("registries", context! {page:"registries",title:"目标仓库"})
}
#[rocket::get("/rules")]
fn rules_page(_admin: AdminSession) -> Template {
    Template::render("rules", context! {page:"rules",title:"镜像规则"})
}
#[rocket::get("/tasks")]
fn tasks_page(_admin: AdminSession) -> Template {
    Template::render("tasks", context! {page:"tasks",title:"同步任务"})
}
#[rocket::get("/tasks/<task_id>")]
fn task_page(_admin: AdminSession, task_id: &str) -> Result<Template> {
    id(task_id)?;
    Ok(Template::render(
        "task_detail",
        context! {page:"task_detail",title:"任务详情",task_id},
    ))
}
#[rocket::get("/health")]
fn health() -> Json<serde_json::Value> {
    Json(json!({"status":"ok"}))
}
pub fn build_rocket(config: AppConfig, store: Store, github: GithubClient) -> Rocket<Build> {
    let assets = config.assets_dir.clone();
    let worker = Worker::new(
        store.clone(),
        Arc::new(github.clone()),
        Arc::new(SystemClock),
    );
    let state = AppState {
        password: Arc::new(Mutex::new(config.password_hash.clone())),
        store,
        config,
        github,
        worker,
        logins: Mutex::new(HashMap::new()),
        inventory_gate: rocket::tokio::sync::Semaphore::new(2),
    };
    let figment = rocket::Config::figment()
        .merge(("template_dir", assets.join("templates")))
        .merge(("limits.json", 128 * 1024u32));
    rocket::custom(figment).manage(state)
        .attach(Template::fairing())
        .attach(AdHoc::try_on_ignite("撤销上次进程的登录会话",|rocket|Box::pin(async move {
            let store=rocket.state::<AppState>().unwrap().store.clone();
            match rocket::tokio::task::spawn_blocking(move||store.revoke_all_sessions()).await {
                Ok(Ok(()))=>Ok(rocket),
                _=>{eprintln!("无法初始化登录会话，拒绝启动");Err(rocket)}
            }
        })))
        .attach(AdHoc::on_liftoff("持久任务队列",|rocket|Box::pin(async move {
            let state=rocket.state::<AppState>().unwrap();
            if !state.config.worker_enabled {return;}
            let worker=state.worker.clone();let shutdown=rocket.shutdown();
            rocket::tokio::spawn(async move {
                let _=rocket::tokio::task::spawn_blocking({let store=worker.store.clone();move||store.recover_dispatching()}).await;
                let mut interval=rocket::tokio::time::interval(std::time::Duration::from_secs(15));
                loop {rocket::tokio::select!{_ = shutdown.clone()=>break,_ = interval.tick()=>{if worker.tick().await.is_err(){eprintln!("后台任务暂时失败，将在下次轮询重试");}}}}
            });
        })))
        .register("/",rocket::catchers![unauthorized,forbidden])
        .mount("/static",FileServer::from(assets.join("static")))
        .mount("/",rocket::routes![health,dashboard,settings_page,registries_page,rules_page,tasks_page,task_page,
            account::page,account::change_password,
            images::page,images::inventory,
            auth::login_page,auth::session,auth::login,auth::logout,
            config::settings,config::save_settings,config::initialize,config::workflow,config::registries,config::create_registry,config::update_registry,config::delete_registry,
            rules::rules,rules::create_rule,rules::update_rule,rules::delete_rule,
            tasks::tasks,tasks::task_page,tasks::enqueue,tasks::task,tasks::logs,tasks::retry,tasks::release])
}

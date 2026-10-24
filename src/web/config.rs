use super::*;
use crate::domain::*;
#[rocket::get("/api/settings/workflow")]
pub fn workflow(_admin: AdminSession) -> rocket::response::content::RawText<&'static str> {
    rocket::response::content::RawText(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/workflows/web-sync.yml"
    )))
}
#[rocket::get("/api/settings")]
pub async fn settings(
    _admin: AdminSession,
    state: &State<AppState>,
) -> Result<Json<Option<SettingsView>>> {
    Ok(Json(db(state, |s| s.settings()).await?))
}
#[rocket::put("/api/settings", data = "<input>")]
pub async fn save_settings(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    input: Json<SettingsInput>,
) -> Result<Json<SettingsView>> {
    let input = input.into_inner();
    Ok(Json(db(state, move |s| s.save_settings(input)).await?))
}
#[rocket::post("/api/settings/initialize")]
pub async fn initialize(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let (settings, revision, token) = db(state, |s| {
        let setup = s.connection_setup()?;
        s.mark_settings_unready(&setup.1)?;
        Ok(setup)
    })
    .await?;
    let connection = ConnectionSnapshot {
        owner: settings.owner,
        repo: settings.repo,
        branch: settings.branch,
        workflow: settings.workflow,
    };
    state
        .github
        .using_token(token)
        .initialize(
            &connection,
            &state.store.crypto.task_secret_name(),
            &state.store.crypto.task_key_hex(),
        )
        .await?;
    db(state, move |s| s.mark_settings_ready(&revision)).await?;
    Ok(Json(
        json!({"ok":true,"message":"连接初始化完成，任务密钥已自动配置，可以开始同步"}),
    ))
}
#[rocket::get("/api/registries")]
pub async fn registries(
    _admin: AdminSession,
    state: &State<AppState>,
) -> Result<Json<Vec<RegistryView>>> {
    Ok(Json(db(state, |s| s.registries()).await?))
}
#[rocket::post("/api/registries", data = "<input>")]
pub async fn create_registry(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    input: Json<RegistryInput>,
) -> Result<Json<RegistryView>> {
    let input = input.into_inner();
    Ok(Json(
        db(state, move |s| s.save_registry(None, input)).await?,
    ))
}
#[rocket::put("/api/registries/<registry_id>", data = "<input>")]
pub async fn update_registry(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    registry_id: &str,
    input: Json<RegistryInput>,
) -> Result<Json<RegistryView>> {
    let id = id(registry_id)?;
    let input = input.into_inner();
    Ok(Json(
        db(state, move |s| s.save_registry(Some(id), input)).await?,
    ))
}
#[rocket::delete("/api/registries/<registry_id>")]
pub async fn delete_registry(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    registry_id: &str,
) -> Result<Json<serde_json::Value>> {
    let id = id(registry_id)?;
    db(state, move |s| s.delete_registry(id)).await?;
    Ok(Json(json!({"ok":true})))
}

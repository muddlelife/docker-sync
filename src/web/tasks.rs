use super::*;
use crate::domain::*;
use serde::Deserialize;
use uuid::Uuid;
#[rocket::get("/api/tasks")]
pub async fn tasks(_admin: AdminSession, state: &State<AppState>) -> Result<Json<Vec<TaskView>>> {
    Ok(Json(db(state, |s| s.tasks()).await?))
}
#[rocket::get("/api/tasks/page?<number>&<size>")]
pub async fn task_page(
    _admin: AdminSession,
    state: &State<AppState>,
    number: Option<u64>,
    size: Option<u32>,
) -> Result<Json<Page<TaskView>>> {
    Ok(Json(
        db(state, move |s| {
            s.task_page(number.unwrap_or(1), size.unwrap_or(10))
        })
        .await?,
    ))
}
#[derive(Deserialize)]
pub struct Enqueue {
    rule_ids: Vec<Uuid>,
    request_key: Uuid,
}
#[rocket::post("/api/tasks", data = "<input>")]
pub async fn enqueue(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    input: Json<Enqueue>,
) -> Result<Json<TaskView>> {
    let input = input.into_inner();
    Ok(Json(
        db(state, move |s| {
            s.enqueue(&input.rule_ids, input.request_key)
        })
        .await?,
    ))
}
#[rocket::get("/api/tasks/<task_id>")]
pub async fn task(
    _admin: AdminSession,
    state: &State<AppState>,
    task_id: &str,
) -> Result<Json<serde_json::Value>> {
    let id = id(task_id)?;
    let warning = state
        .worker
        .refresh_terminal(id)
        .await
        .err()
        .map(|e| e.to_string());
    let task = db(state, move |s| s.task(id)).await?;
    let attempt = task.run_attempt;
    let result = db(state, move |s| s.result(id, attempt)).await?;
    Ok(Json(json!({"task":task,"result":result,"warning":warning})))
}
#[rocket::get("/api/tasks/<task_id>/logs")]
pub async fn logs(
    _admin: AdminSession,
    state: &State<AppState>,
    task_id: &str,
) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({"logs":state.worker.logs(id(task_id)?).await?})))
}
#[rocket::post("/api/tasks/<task_id>/retry")]
pub async fn retry(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    task_id: &str,
) -> Result<Json<TaskView>> {
    let id = id(task_id)?;
    Ok(Json(db(state, move |s| s.retry(id)).await?))
}
#[derive(Deserialize)]
pub struct Release {
    confirm: bool,
}
#[rocket::post("/api/tasks/<task_id>/release", data = "<input>")]
pub async fn release(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    task_id: &str,
    input: Json<Release>,
) -> Result<Json<serde_json::Value>> {
    if !input.confirm {
        return Err(Error::invalid("需要明确确认 GitHub 可能仍在执行"));
    }
    let id = id(task_id)?;
    db(state, move |s| s.release_unknown(id)).await?;
    Ok(Json(json!({"ok":true})))
}

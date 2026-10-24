use super::*;
use crate::domain::*;
#[rocket::get("/api/rules")]
pub async fn rules(_admin: AdminSession, state: &State<AppState>) -> Result<Json<Vec<RuleView>>> {
    state.worker.refresh_rules().await?;
    Ok(Json(db(state, |s| s.rules()).await?))
}
#[rocket::post("/api/rules", data = "<input>")]
pub async fn create_rule(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    input: Json<RuleInput>,
) -> Result<Json<RuleView>> {
    let input = input.into_inner();
    Ok(Json(db(state, move |s| s.save_rule(None, input)).await?))
}
#[rocket::put("/api/rules/<rule_id>", data = "<input>")]
pub async fn update_rule(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    rule_id: &str,
    input: Json<RuleInput>,
) -> Result<Json<RuleView>> {
    let id = id(rule_id)?;
    let input = input.into_inner();
    Ok(Json(
        db(state, move |s| s.save_rule(Some(id), input)).await?,
    ))
}
#[rocket::delete("/api/rules/<rule_id>")]
pub async fn delete_rule(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    rule_id: &str,
) -> Result<Json<serde_json::Value>> {
    let id = id(rule_id)?;
    db(state, move |s| s.delete_rule(id)).await?;
    Ok(Json(json!({"ok":true})))
}

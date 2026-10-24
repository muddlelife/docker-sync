use super::*;
use crate::bootstrap::Bootstrap;
use rocket::http::{Cookie, CookieJar};
use serde::Deserialize;

#[rocket::get("/account")]
pub fn page(_admin: AdminSession) -> Template {
    Template::render("account", context! {page:"account",title:"账户安全"})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordChange {
    current_password: String,
    new_password: String,
    confirm_password: String,
}

#[rocket::post("/api/account/password", data = "<input>")]
pub async fn change_password(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    ip: auth::ClientIp,
    state: &State<AppState>,
    jar: &CookieJar<'_>,
    input: Json<PasswordChange>,
) -> Result<Json<serde_json::Value>> {
    let input = input.into_inner();
    if !(12..=128).contains(&input.new_password.chars().count()) {
        return Err(Error::invalid("新密码需为 12 至 128 个字符"));
    }
    if input.new_password != input.confirm_password {
        return Err(Error::invalid("两次输入的新密码不一致"));
    }
    if input.new_password == input.current_password {
        return Err(Error::invalid("新密码不能与当前密码相同"));
    }
    auth::limit_attempt(state, &ip)?;
    let gate = state.password.clone();
    let store = state.store.clone();
    let dir = state.config.data_dir.clone();
    rocket::tokio::task::spawn_blocking(move || -> Result<()> {
        // The whole operation remains serialized even if the HTTP client disconnects.
        let mut hash = gate.lock().map_err(|_| Error::Internal("密码状态不可用"))?;
        if !auth::verify_password(&input.current_password, &hash) {
            return Err(Error::invalid("当前密码不正确"));
        }
        let next = hash_password(&input.new_password)?;
        store.revoke_all_sessions()?;
        if let Err(error) = Bootstrap::change_password(&dir, input.new_password) {
            // A directory fsync failure may follow a successful rename: reload the
            // actual credential, or fail closed if it cannot be read.
            *hash = Bootstrap::load(&dir)
                .and_then(|setup| hash_password(&setup.admin_password))
                .unwrap_or_default();
            return Err(error);
        }
        *hash = next;
        Ok(())
    })
    .await
    .map_err(|_| Error::Internal("密码修改失败"))??;
    jar.remove(Cookie::build("docker_sync_session").path("/").build());
    Ok(Json(
        json!({"ok":true,"message":"密码已修改，请使用新密码重新登录"}),
    ))
}

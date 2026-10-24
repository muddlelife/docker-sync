use super::*;
use crate::store::{Session, now};
use argon2::{PasswordHash, PasswordVerifier};
use rocket::{
    http::{Cookie, CookieJar, SameSite},
    request::{FromRequest, Outcome},
};
use serde::Deserialize;

pub struct AdminSession;
pub struct CsrfGuard;
pub struct ClientIp(String);
pub(super) fn limit_attempt(state: &AppState, ip: &ClientIp) -> Result<()> {
    let mut rate = state
        .logins
        .lock()
        .map_err(|_| Error::Internal("登录限制状态不可用"))?;
    rate.retain(|_, v| {
        v.retain(|t| *t > now() - 60);
        !v.is_empty()
    });
    let global: usize = rate.values().map(Vec::len).sum();
    if global >= 30 || rate.get(&ip.0).is_some_and(|v| v.len() >= 5) {
        return Err(Error::RateLimited(60));
    }
    rate.entry(ip.0.clone()).or_default().push(now());
    Ok(())
}
pub(super) fn verify_password(password: &str, hash: &str) -> bool {
    password.len() <= 1024
        && PasswordHash::new(hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
}
async fn request_session(request: &Request<'_>) -> Option<Session> {
    let state = request.rocket().state::<AppState>()?;
    let token = request
        .cookies()
        .get("docker_sync_session")?
        .value()
        .to_owned();
    db(state, move |s| s.session(&token)).await.ok().flatten()
}
#[rocket::async_trait]
impl<'r> FromRequest<'r> for AdminSession {
    type Error = ();
    async fn from_request(r: &'r Request<'_>) -> Outcome<Self, ()> {
        if request_session(r).await.is_some_and(|s| s.authenticated) {
            Outcome::Success(Self)
        } else {
            Outcome::Error((Status::Unauthorized, ()))
        }
    }
}
#[rocket::async_trait]
impl<'r> FromRequest<'r> for CsrfGuard {
    type Error = ();
    async fn from_request(r: &'r Request<'_>) -> Outcome<Self, ()> {
        if request_session(r)
            .await
            .is_some_and(|s| r.headers().get_one("X-CSRF-Token") == Some(s.csrf.as_str()))
        {
            Outcome::Success(Self)
        } else {
            Outcome::Error((Status::Forbidden, ()))
        }
    }
}
#[rocket::async_trait]
impl<'r> FromRequest<'r> for ClientIp {
    type Error = ();
    async fn from_request(r: &'r Request<'_>) -> Outcome<Self, ()> {
        Outcome::Success(Self(
            r.remote()
                .map(|e| e.ip().to_string())
                .unwrap_or_else(|| "local".into()),
        ))
    }
}
fn cookie(jar: &CookieJar<'_>, state: &AppState, session: &Session) {
    jar.add(
        Cookie::build(("docker_sync_session", session.token.clone()))
            .path("/")
            .http_only(true)
            .secure(state.config.secure_cookie)
            .same_site(SameSite::Strict)
            .max_age(rocket::time::Duration::seconds(
                (session.expires - now()).max(0),
            ))
            .build(),
    );
}
#[rocket::get("/login")]
pub async fn login_page(state: &State<AppState>, jar: &CookieJar<'_>) -> Result<Template> {
    let token = jar
        .get("docker_sync_session")
        .map(|c| c.value().to_string());
    let existing = if let Some(token) = token {
        db(state, move |s| s.session(&token)).await?
    } else {
        None
    };
    let session = match existing {
        Some(s) => s,
        None => db(state, |s| s.create_session(false)).await?,
    };
    cookie(jar, state, &session);
    Ok(Template::render(
        "login",
        context! {csrf:session.csrf,title:"登录",page:"login"},
    ))
}
#[rocket::get("/api/session")]
pub async fn session(
    state: &State<AppState>,
    jar: &CookieJar<'_>,
) -> Result<Json<serde_json::Value>> {
    let Some(token) = jar
        .get("docker_sync_session")
        .map(|c| c.value().to_string())
    else {
        return Err(Error::Unauthorized);
    };
    let s = db(state, move |s| s.session(&token))
        .await?
        .ok_or(Error::Unauthorized)?;
    Ok(Json(json!({"authenticated":s.authenticated,"csrf":s.csrf})))
}
#[derive(Deserialize)]
pub struct Login {
    password: String,
}
#[rocket::post("/api/login", data = "<input>")]
pub async fn login(
    _csrf: CsrfGuard,
    ip: ClientIp,
    state: &State<AppState>,
    jar: &CookieJar<'_>,
    input: Json<Login>,
) -> Result<Json<serde_json::Value>> {
    limit_attempt(state, &ip)?;
    let password = input.into_inner().password;
    let gate = state.password.clone();
    let store = state.store.clone();
    let old = jar
        .get("docker_sync_session")
        .map(|c| c.value().to_string());
    let session = rocket::tokio::task::spawn_blocking(move || -> Result<Session> {
        // Hold through session creation so password replacement revokes every older login.
        let hash = gate.lock().map_err(|_| Error::Internal("密码状态不可用"))?;
        if !verify_password(&password, &hash) {
            return Err(Error::Unauthorized);
        }
        if let Some(old) = old {
            store.delete_session(&old)?;
        }
        store.create_session(true)
    })
    .await
    .map_err(|_| Error::Internal("密码校验失败"))??;
    cookie(jar, state, &session);
    Ok(Json(json!({"ok":true})))
}
#[rocket::post("/api/logout")]
pub async fn logout(
    _admin: AdminSession,
    _csrf: CsrfGuard,
    state: &State<AppState>,
    jar: &CookieJar<'_>,
) -> Result<Json<serde_json::Value>> {
    if let Some(token) = jar
        .get("docker_sync_session")
        .map(|c| c.value().to_string())
    {
        db(state, move |s| s.delete_session(&token)).await?;
    }
    jar.remove(Cookie::build("docker_sync_session").path("/").build());
    Ok(Json(json!({"ok":true})))
}

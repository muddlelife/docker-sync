use docker_sync::{
    bootstrap::Bootstrap,
    crypto::Crypto,
    github::GithubClient,
    store::Store,
    web::{AppConfig, build_rocket},
};
use rocket::{
    http::{ContentType, Cookie, Header, Status},
    local::asynchronous::Client,
};
use serde_json::{Value, json};

const OLD: &str = "original-admin-password";
const NEW: &str = "replacement-admin-password";

async fn app(dir: &std::path::Path) -> Client {
    let (setup, _) = Bootstrap::load_or_create(dir).unwrap();
    let store = Store::open(
        dir.join("docker-sync.db"),
        Crypto::new(
            Crypto::key_from_hex(&setup.app_encryption_key).unwrap(),
            Crypto::key_from_hex(&setup.sync_payload_key).unwrap(),
        ),
    )
    .unwrap();
    let mut config = AppConfig::for_password(&setup.admin_password).unwrap();
    config.data_dir = dir.into();
    config.secure_cookie = false;
    config.worker_enabled = false;
    Client::tracked(build_rocket(
        config,
        store,
        GithubClient::new("fixture".into(), "http://127.0.0.1:9").unwrap(),
    ))
    .await
    .unwrap()
}

async fn fixture() -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let (mut setup, _) = Bootstrap::load_or_create(dir.path()).unwrap();
    setup.admin_password = OLD.into();
    std::fs::write(
        dir.path().join("bootstrap.json"),
        serde_json::to_vec(&setup).unwrap(),
    )
    .unwrap();
    let client = app(dir.path()).await;
    (dir, client)
}

async fn csrf(client: &Client) -> String {
    let session: Value = client
        .get("/api/session")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    session["csrf"].as_str().unwrap().into()
}

async fn login(client: &Client, password: &str) -> Status {
    client.get("/login").dispatch().await;
    client
        .post("/api/login")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf(client).await))
        .body(json!({"password":password}).to_string())
        .dispatch()
        .await
        .status()
}

fn input(current: &str, new: &str, confirmation: &str) -> Value {
    json!({"current_password":current,"new_password":new,"confirm_password":confirmation})
}

#[rocket::async_test]
async fn password_change_requires_login_and_csrf() {
    let (_dir, client) = fixture().await;
    assert_eq!(
        client
            .post("/api/account/password")
            .header(ContentType::JSON)
            .body(input(OLD, NEW, NEW).to_string())
            .dispatch()
            .await
            .status(),
        Status::Unauthorized
    );
    assert_eq!(login(&client, OLD).await, Status::Ok);
    assert_eq!(
        client
            .post("/api/account/password")
            .header(ContentType::JSON)
            .body(input(OLD, NEW, NEW).to_string())
            .dispatch()
            .await
            .status(),
        Status::Forbidden
    );
    assert_eq!(client.get("/account").dispatch().await.status(), Status::Ok);
}

#[rocket::async_test]
async fn invalid_password_changes_preserve_credentials_and_login() {
    let (dir, client) = fixture().await;
    assert_eq!(login(&client, OLD).await, Status::Ok);
    let csrf = csrf(&client).await;
    let before = std::fs::read(dir.path().join("bootstrap.json")).unwrap();
    let long = "x".repeat(1025);
    for body in [
        input("incorrect-password", NEW, NEW),
        input(OLD, NEW, "different-password"),
        input(OLD, "short", "short"),
        input(OLD, OLD, OLD),
        input(OLD, &long, &long),
    ] {
        assert_eq!(
            client
                .post("/api/account/password")
                .header(ContentType::JSON)
                .header(Header::new("X-CSRF-Token", csrf.clone()))
                .body(body.to_string())
                .dispatch()
                .await
                .status(),
            Status::UnprocessableEntity
        );
    }
    assert_eq!(
        client.get("/api/rules").dispatch().await.status(),
        Status::Ok
    );
    assert_eq!(
        std::fs::read(dir.path().join("bootstrap.json")).unwrap(),
        before
    );
    assert_eq!(login(&client, OLD).await, Status::Ok);
}

#[rocket::async_test]
async fn changing_password_is_immediate_persistent_and_revokes_every_session() {
    let (dir, client) = fixture().await;
    let (before, _) = Bootstrap::load_or_create(dir.path()).unwrap();
    assert_eq!(login(&client, OLD).await, Status::Ok);
    let old_cookie = client
        .cookies()
        .get("docker_sync_session")
        .unwrap()
        .value()
        .to_owned();
    let store = client
        .rocket()
        .state::<docker_sync::web::AppState>()
        .unwrap()
        .store
        .clone();
    let second = store.create_session(true).unwrap();
    let response = client
        .post("/api/account/password")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf(&client).await))
        .body(input(OLD, NEW, NEW).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let body = response.into_string().await.unwrap();
    assert!(!body.contains(OLD) && !body.contains(NEW));
    for token in [old_cookie, second.token] {
        assert!(store.session(&token).unwrap().is_none());
        assert_eq!(
            client
                .get("/api/rules")
                .cookie(Cookie::new("docker_sync_session", token))
                .dispatch()
                .await
                .status(),
            Status::Unauthorized
        );
    }
    let (after, _) = Bootstrap::load_or_create(dir.path()).unwrap();
    assert_eq!(after.admin_password, NEW);
    assert_eq!(after.app_encryption_key, before.app_encryption_key);
    assert_eq!(after.sync_payload_key, before.sync_payload_key);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("bootstrap.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(login(&client, OLD).await, Status::Unauthorized);
    assert_eq!(login(&client, NEW).await, Status::Ok);
    drop(client);
    let restarted = app(dir.path()).await;
    assert_eq!(login(&restarted, OLD).await, Status::Unauthorized);
    assert_eq!(login(&restarted, NEW).await, Status::Ok);
}

#[rocket::async_test]
async fn repeated_current_password_guesses_share_the_login_rate_limit() {
    let (_dir, client) = fixture().await;
    assert_eq!(login(&client, OLD).await, Status::Ok);
    let csrf = csrf(&client).await;
    for _ in 0..4 {
        assert_eq!(
            client
                .post("/api/account/password")
                .header(ContentType::JSON)
                .header(Header::new("X-CSRF-Token", csrf.clone()))
                .body(input("wrong-password", NEW, NEW).to_string())
                .dispatch()
                .await
                .status(),
            Status::UnprocessableEntity
        );
    }
    assert_eq!(
        client
            .post("/api/account/password")
            .header(ContentType::JSON)
            .header(Header::new("X-CSRF-Token", csrf))
            .body(input("wrong-password", NEW, NEW).to_string())
            .dispatch()
            .await
            .status(),
        Status::TooManyRequests
    );
}

#[rocket::async_test]
async fn concurrent_password_changes_cannot_both_accept_the_old_password() {
    let (dir, client) = fixture().await;
    assert_eq!(login(&client, OLD).await, Status::Ok);
    let csrf = csrf(&client).await;
    let one = client
        .post("/api/account/password")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.clone()))
        .body(input(OLD, NEW, NEW).to_string());
    let other = "second-replacement-password";
    let two = client
        .post("/api/account/password")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf))
        .body(input(OLD, other, other).to_string());
    let (one, two) = rocket::tokio::join!(one.dispatch(), two.dispatch());
    let statuses = [one.status(), two.status()];
    assert_eq!(statuses.iter().filter(|&&s| s == Status::Ok).count(), 1);
    assert!(statuses.iter().all(|s| {
        [
            Status::Ok,
            Status::Unauthorized,
            Status::UnprocessableEntity,
        ]
        .contains(s)
    }));
    let expected = if one.status() == Status::Ok {
        NEW
    } else {
        other
    };
    assert_eq!(
        Bootstrap::load_or_create(dir.path())
            .unwrap()
            .0
            .admin_password,
        expected
    );
    assert_eq!(login(&client, expected).await, Status::Ok);
}

#[rocket::async_test]
async fn failed_password_persistence_keeps_the_existing_credential() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, client) = fixture().await;
    assert_eq!(login(&client, OLD).await, Status::Ok);
    let csrf = csrf(&client).await;
    let before = std::fs::read(dir.path().join("bootstrap.json")).unwrap();
    // Keep WAL files present so read-only directory permissions fail the
    // credential file replacement rather than the authentication DB lookup.
    let database = rusqlite::Connection::open(dir.path().join("docker-sync.db")).unwrap();
    database
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let status = client
        .post("/api/account/password")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf))
        .body(input(OLD, NEW, NEW).to_string())
        .dispatch()
        .await
        .status();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(database);
    assert_eq!(status, Status::InternalServerError);
    assert_eq!(
        std::fs::read(dir.path().join("bootstrap.json")).unwrap(),
        before
    );
    assert_eq!(login(&client, OLD).await, Status::Ok);
    assert_eq!(login(&client, NEW).await, Status::Unauthorized);
}

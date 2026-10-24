use docker_sync::{
    crypto::Crypto,
    github::GithubClient,
    store::Store,
    web::{AppConfig, build_rocket},
};
use rocket::{
    http::{ContentType, Header, Status},
    local::asynchronous::Client,
};
use serde_json::{Value, json};

async fn client() -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    let mut config = AppConfig::for_password("a-strong-test-password").unwrap();
    config.worker_enabled = false;
    config.secure_cookie = false;
    let app = build_rocket(
        config,
        store,
        GithubClient::new("token".into(), "http://127.0.0.1:9").unwrap(),
    );
    (dir, Client::tracked(app).await.unwrap())
}
async fn login(client: &Client) -> String {
    let response = client.get("/login").dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let session: Value = client
        .get("/api/session")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    let csrf = session["csrf"].as_str().unwrap();
    let response = client
        .post("/api/login")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.to_owned()))
        .body(json!({"password":"a-strong-test-password"}).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let session: Value = client
        .get("/api/session")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    session["csrf"].as_str().unwrap().into()
}
#[rocket::async_test]
async fn unauthenticated_access_and_authenticated_csrf_are_enforced() {
    let (_dir, client) = client().await;
    assert_eq!(
        client.get("/api/registries").dispatch().await.status(),
        Status::Unauthorized
    );
    let csrf = login(&client).await;
    let body =
        json!({"owner":"owner","repo":"repo","branch":"main","token":"private-token"}).to_string();
    assert_eq!(
        client
            .put("/api/settings")
            .header(ContentType::JSON)
            .body(body.clone())
            .dispatch()
            .await
            .status(),
        Status::Forbidden
    );
    let response = client
        .put("/api/settings")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.clone()))
        .body(body)
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    assert!(
        !response
            .into_string()
            .await
            .unwrap()
            .contains("private-token")
    );
    assert_eq!(
        client
            .post("/api/logout")
            .header(Header::new("X-CSRF-Token", csrf))
            .dispatch()
            .await
            .status(),
        Status::Ok
    );
    assert_eq!(
        client.get("/api/settings").dispatch().await.status(),
        Status::Unauthorized
    );
}
#[rocket::async_test]
async fn rule_page_exposes_platforms_without_returning_registry_passwords() {
    let (_dir, client) = client().await;
    let csrf = login(&client).await;
    let response = client.get("/rules").dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let html = response.into_string().await.unwrap();
    assert!(html.contains("linux/amd64"));
    assert!(html.contains("linux/arm64"));
    let registry:Value=client.post("/api/registries").header(ContentType::JSON).header(Header::new("X-CSRF-Token",csrf.clone())).body(json!({"name":"ACR","provider":"aliyun_acr","endpoint":"registry.example.com","namespace":"demo","username":"user","password":"private-password","enabled":true}).to_string()).dispatch().await.into_json().await.unwrap();
    assert!(registry.get("password").is_none());
    let response = client
        .post("/api/rules/import/preview")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.clone()))
        .body(json!({"registry_id":registry["id"],"content":"nginx:latest"}).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NotFound);
    assert_eq!(
        client
            .post("/api/rules/import")
            .header(ContentType::JSON)
            .header(Header::new("X-CSRF-Token", csrf))
            .body(json!({"rules":[]}).to_string())
            .dispatch()
            .await
            .status(),
        Status::NotFound
    );
    let rules: Value = client
        .get("/api/rules")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    assert_eq!(rules.as_array().unwrap().len(), 0);
}

#[rocket::async_test]
async fn password_reset_and_restart_revoke_existing_authenticated_cookie() {
    let (dir, old) = client().await;
    login(&old).await;
    let cookie = old
        .cookies()
        .get("docker_sync_session")
        .unwrap()
        .clone()
        .into_owned();
    drop(old);
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    let mut config = AppConfig::for_password("a-new-administrator-password").unwrap();
    config.worker_enabled = false;
    config.secure_cookie = false;
    let new = Client::tracked(build_rocket(
        config,
        store,
        GithubClient::new("token".into(), "http://127.0.0.1:9").unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(
        new.get("/api/settings")
            .cookie(cookie)
            .dispatch()
            .await
            .status(),
        Status::Unauthorized
    );
}

#[rocket::async_test]
async fn github_initialization_requires_admin_and_csrf_without_revealing_secrets() {
    let (_dir, client) = client().await;
    assert_eq!(
        client
            .post("/api/settings/initialize")
            .dispatch()
            .await
            .status(),
        Status::Unauthorized
    );
    let csrf = login(&client).await;
    assert_eq!(
        client
            .post("/api/settings/initialize")
            .dispatch()
            .await
            .status(),
        Status::Forbidden
    );
    assert_eq!(
        client
            .post("/api/settings/initialize")
            .header(Header::new("X-CSRF-Token", csrf))
            .dispatch()
            .await
            .status(),
        Status::UnprocessableEntity
    );
    assert_eq!(
        client
            .post("/api/settings/task-key")
            .dispatch()
            .await
            .status(),
        Status::NotFound
    );
}

#[rocket::async_test]
async fn administrator_can_download_the_standalone_sync_workflow() {
    let (_dir, client) = client().await;
    assert_eq!(
        client
            .get("/api/settings/workflow")
            .dispatch()
            .await
            .status(),
        Status::Unauthorized
    );
    login(&client).await;
    let response = client.get("/api/settings/workflow").dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.content_type(), Some(ContentType::Plain));
    let expected = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/workflows/web-sync.yml"
    ))
    .unwrap();
    assert_eq!(response.into_string().await.unwrap(), expected);
}

#[rocket::async_test]
async fn remote_image_queries_require_login_before_accessing_any_registry() {
    let (_dir, c) = client().await;
    let url = "/api/registries/00000000-0000-0000-0000-000000000001/images?repository=nginx";
    assert_eq!(c.get(url).dispatch().await.status(), Status::Unauthorized);
    c.get("/login").dispatch().await;
    assert_eq!(c.get(url).dispatch().await.status(), Status::Unauthorized);
    login(&c).await;
    assert_eq!(c.get(url).dispatch().await.status(), Status::NotFound);
    assert_eq!(c.get("/images").dispatch().await.status(), Status::Ok);
}

#[rocket::async_test]
async fn every_management_api_rejects_anonymous_access_and_mutations_require_csrf() {
    use rocket::http::Method;
    let (_dir, c) = client().await;
    let id = "00000000-0000-0000-0000-000000000001";
    let reads = vec![
        "/api/settings".into(),
        "/api/settings/workflow".into(),
        "/api/registries".into(),
        "/api/rules".into(),
        "/api/tasks".into(),
        format!("/api/tasks/{id}"),
        format!("/api/tasks/{id}/logs"),
        format!("/api/registries/{id}/images"),
    ];
    let writes = vec![
        (Method::Put, "/api/settings".into()),
        (Method::Post, "/api/settings/initialize".into()),
        (Method::Post, "/api/registries".into()),
        (Method::Put, format!("/api/registries/{id}")),
        (Method::Delete, format!("/api/registries/{id}")),
        (Method::Post, "/api/rules".into()),
        (Method::Put, format!("/api/rules/{id}")),
        (Method::Delete, format!("/api/rules/{id}")),
        (Method::Post, "/api/tasks".into()),
        (Method::Post, format!("/api/tasks/{id}/retry")),
        (Method::Post, format!("/api/tasks/{id}/release")),
        (Method::Post, "/api/logout".into()),
    ];
    for anonymous_session in [false, true] {
        if anonymous_session {
            c.get("/login").dispatch().await;
        }
        for url in &reads {
            assert_eq!(
                c.get(url).dispatch().await.status(),
                Status::Unauthorized,
                "{url}"
            );
        }
        for (method, url) in &writes {
            assert_eq!(
                c.req(*method, url)
                    .header(ContentType::JSON)
                    .body("{}")
                    .dispatch()
                    .await
                    .status(),
                Status::Unauthorized,
                "{url}"
            );
        }
    }
    login(&c).await;
    for (method, url) in &writes {
        assert_eq!(
            c.req(*method, url)
                .header(ContentType::JSON)
                .body("{}")
                .dispatch()
                .await
                .status(),
            Status::Forbidden,
            "{url}"
        );
    }
}

#[rocket::async_test]
async fn rule_page_refresh_detects_external_rerun_without_opening_task_details() {
    use docker_sync::domain::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    store
        .save_settings(SettingsInput {
            owner: "owner".into(),
            repo: "repo".into(),
            branch: "main".into(),
            token: Some("token".into()),
        })
        .unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let reg = store
        .save_registry(
            None,
            RegistryInput {
                name: "ACR".into(),
                provider: "aliyun_acr".into(),
                endpoint: "registry.example.com".into(),
                namespace: "demo".into(),
                username: "user".into(),
                password: Some("password".into()),
                enabled: true,
            },
        )
        .unwrap();
    let rule = store
        .save_rule(
            None,
            RuleInput {
                name: "nginx".into(),
                source: "nginx".into(),
                registry_id: reg.id,
                target_repository: "nginx".into(),
                target_tag: "latest".into(),
                platform: Platform::All,
                enabled: true,
                source_username: None,
                source_password: None,
            },
        )
        .unwrap();
    let task = store.enqueue(&[rule.id], uuid::Uuid::new_v4()).unwrap();
    store
        .mutate(task.id, |t| {
            t.status = TaskStatus::Succeeded;
            t.run_id = Some(42);
        })
        .unwrap();
    Mock::given(path("/repos/owner/repo/actions/runs/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id":42,"run_attempt":2,"status":"in_progress","conclusion":null}),
        ))
        .mount(&server)
        .await;
    let mut cfg = AppConfig::for_password("a-strong-test-password").unwrap();
    cfg.worker_enabled = false;
    cfg.secure_cookie = false;
    let c = Client::tracked(build_rocket(
        cfg,
        store.clone(),
        GithubClient::new("token".into(), &server.uri()).unwrap(),
    ))
    .await
    .unwrap();
    login(&c).await;
    let rows: Value = c
        .get("/api/rules")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    assert_eq!(rows[0]["sync"]["status"], "queued");
    assert_eq!(store.task(task.id).unwrap().run_attempt, 2);
    assert!(rows[0]["sync"]["elapsed_ms"].is_null());
}
#[rocket::async_test]
async fn unsuccessful_reinitialization_clears_previous_ready_state() {
    use docker_sync::{domain::SettingsInput, web::AppState};
    let (_dir, c) = client().await;
    let store = &c.rocket().state::<AppState>().unwrap().store;
    store
        .save_settings(SettingsInput {
            owner: "owner".into(),
            repo: "repo".into(),
            branch: "main".into(),
            token: Some("token".into()),
        })
        .unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let csrf = login(&c).await;
    let response = c
        .post("/api/settings/initialize")
        .header(Header::new("X-CSRF-Token", csrf))
        .dispatch()
        .await;
    assert_ne!(response.status(), Status::Ok);
    assert!(!store.settings().unwrap().unwrap().ready);
}

#[rocket::async_test]
async fn paged_task_history_requires_login_and_returns_page_metadata() {
    let (_dir, c) = client().await;
    assert_eq!(
        c.get("/api/tasks/page?number=1&size=10")
            .dispatch()
            .await
            .status(),
        Status::Unauthorized
    );
    login(&c).await;
    let response = c.get("/api/tasks/page?number=1&size=10").dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let page: Value = response.into_json().await.unwrap();
    assert_eq!(page["total"], 0);
    assert_eq!(page["page"], 1);
    assert_eq!(page["page_size"], 10);
    assert_eq!(page["pages"], 1);
    assert_eq!(
        c.get("/api/tasks/page?number=1&size=101")
            .dispatch()
            .await
            .status(),
        Status::UnprocessableEntity
    );
}

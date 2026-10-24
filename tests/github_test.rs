use docker_sync::{
    domain::*,
    github::{DispatchError, GithubApi, GithubClient},
};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};
fn connection() -> ConnectionSnapshot {
    ConnectionSnapshot {
        owner: "owner".into(),
        repo: "repo".into(),
        branch: "main".into(),
        workflow: "web-sync.yml".into(),
    }
}
async fn valid_workflow_contents(server: &MockServer) {
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer test-token"))
        .and(|r: &wiremock::Request| {
            r.headers.get_all("accept").iter().count() == 1
                && r.headers
                    .get("accept")
                    .is_some_and(|v| v == "application/vnd.github.raw+json")
        })
        .and(path(
            "/repos/owner/repo/contents/.github/workflows/web-sync.yml",
        ))
        .and(wiremock::matchers::query_param("ref", "main"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(include_str!("../workflows/web-sync.yml")),
        )
        .mount(server)
        .await;
}
#[rocket::async_test]
async fn dispatch_uses_versioned_api_and_preserves_exact_run_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/repos/owner/repo/actions/workflows/web-sync.yml/dispatches"))
        .and(header("X-GitHub-Api-Version","2026-03-10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"workflow_run_id":42,"run_url":"https://api.github.com/run/42","html_url":"https://github.com/owner/repo/actions/runs/42"}))).mount(&server).await;
    let client = GithubClient::new("not-public".into(), &server.uri()).unwrap();
    let run = client
        .dispatch(
            &connection(),
            Uuid::new_v4(),
            "ciphertext",
            "DOCKER_SYNC_KEY_TEST",
        )
        .await
        .unwrap();
    assert_eq!(run.id, 42);
}
#[rocket::async_test]
async fn rejected_and_ambiguous_dispatches_have_different_recovery_paths() {
    for (code, unknown) in [(401, false), (422, false), (500, true)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(code).set_body_string("secret-server-message"))
            .mount(&server)
            .await;
        let client = GithubClient::new("not-public".into(), &server.uri()).unwrap();
        let error = client
            .dispatch(
                &connection(),
                Uuid::new_v4(),
                "ciphertext",
                "DOCKER_SYNC_KEY_TEST",
            )
            .await
            .unwrap_err();
        assert_eq!(matches!(error, DispatchError::Unknown), unknown);
        assert!(!format!("{error:?}").contains("secret-server-message"));
    }
}
#[rocket::async_test]
async fn reconciliation_matches_exact_task_name_instead_of_latest_run() {
    let server = MockServer::start().await;
    let id = Uuid::new_v4();
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"workflow_runs":[
        {"id":99,"display_title":"new unrelated task","event":"workflow_dispatch","head_branch":"main","run_attempt":1},
        {"id":42,"display_title":format!("image-sync-{id}"),"event":"workflow_dispatch","head_branch":"main","run_attempt":1}
    ]}))).mount(&server).await;
    let client = GithubClient::new("not-public".into(), &server.uri()).unwrap();
    assert_eq!(
        client
            .find_run(&connection(), id, 0)
            .await
            .unwrap()
            .unwrap()
            .id,
        42
    );
}

fn archive(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        writer
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
async fn result_client(bytes: Vec<u8>) -> (MockServer, MockServer, GithubClient) {
    let api = MockServer::start().await;
    let download = MockServer::start().await;
    Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/42/artifacts")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"artifacts":[{"id":9,"name":"image-sync-result-1","expired":false}]}))).mount(&api).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/artifacts/9/zip"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/download", download.uri())),
        )
        .mount(&api)
        .await;
    Mock::given(wiremock::matchers::header_exists("authorization"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&download)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .with_priority(2)
        .mount(&download)
        .await;
    let client = GithubClient::new("not-public".into(), &api.uri()).unwrap();
    (api, download, client)
}
#[rocket::async_test]
async fn artifact_redirect_never_forwards_api_token_to_storage() {
    let id = Uuid::new_v4();
    let result=serde_json::to_vec(&serde_json::json!({"version":1,"task_id":id,"run_attempt":1,"items":[],"summary":{"synced":0,"unchanged":0,"failed":0}})).unwrap();
    let (_api, _download, client) = result_client(archive(&[("result.json", result)])).await;
    assert_eq!(
        client
            .download_result(&connection(), 42, 1)
            .await
            .unwrap()
            .task_id,
        id
    );
}
#[rocket::async_test]
async fn unsafe_zip_paths_and_expansion_are_rejected_before_result_parsing() {
    for files in [
        vec![("../result.json", b"{}".to_vec())],
        vec![("huge.bin", vec![0; 21 * 1024 * 1024])],
    ] {
        let (_api, _download, client) = result_client(archive(&files)).await;
        assert!(client.download_result(&connection(), 42, 1).await.is_err());
    }
}

#[rocket::async_test]
async fn initialization_seals_instance_key_for_github_without_plaintext_upload() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let server = MockServer::start().await;
    valid_workflow_contents(&server).await;
    let secret = crypto_box::SecretKey::from_bytes([7; 32]);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/workflows/web-sync.yml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"state":"active","name":"Web 镜像同步 v2"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/repos/owner/repo/actions/secrets/public-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"key_id":"123","key":STANDARD.encode(secret.public_key().as_bytes())}))).mount(&server).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let crypto = docker_sync::crypto::Crypto::new([1; 32], [2; 32]);
    let name = crypto.task_secret_name();
    let client = GithubClient::new("test-token".into(), &server.uri()).unwrap();
    client
        .initialize(&connection(), &name, &crypto.task_key_hex())
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let put = requests.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(
        put.url.path(),
        format!("/repos/owner/repo/actions/secrets/{name}")
    );
    let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
    assert_eq!(body["key_id"], "123");
    let cipher = STANDARD
        .decode(body["encrypted_value"].as_str().unwrap())
        .unwrap();
    assert_eq!(secret.unseal(&cipher).unwrap(), b"02".repeat(32));
    assert!(!String::from_utf8_lossy(&put.body).contains(&"02".repeat(32)));
    assert_ne!(
        name,
        docker_sync::crypto::Crypto::new([1; 32], [3; 32]).task_secret_name()
    );
}
#[rocket::async_test]
async fn secret_permission_failure_has_actionable_sanitized_error() {
    let server = MockServer::start().await;
    valid_workflow_contents(&server).await;
    Mock::given(path("/repos/owner/repo/actions/workflows/web-sync.yml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"state":"active","name":"Web 镜像同步 v2"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/repos/owner/repo/actions/secrets/public-key"))
        .respond_with(ResponseTemplate::new(403).set_body_string("PRIVATE-UPSTREAM-CREDENTIAL"))
        .mount(&server)
        .await;
    let client = GithubClient::new("test-token".into(), &server.uri()).unwrap();
    let message = client
        .initialize(&connection(), "DOCKER_SYNC_KEY_TEST", &"02".repeat(32))
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("Secrets"));
    assert!(!message.contains("PRIVATE-UPSTREAM-CREDENTIAL"));
}

#[rocket::async_test]
async fn initialization_rejects_incompatible_workflow_on_execution_branch_before_writing_secret() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/owner/repo/actions/workflows/web-sync.yml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"state":"active","name":"Web 镜像同步 v2"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/repos/owner/repo/contents/.github/workflows/web-sync.yml"))
      .and(wiremock::matchers::query_param("ref","old-branch"))
      .respond_with(ResponseTemplate::new(200).set_body_string("name: Web 镜像同步 v2\non:\n  workflow_dispatch:\n    inputs:\n      payload:\n        required: true\n        type: string\n")).mount(&server).await;
    let public = crypto_box::SecretKey::from_bytes([7; 32]).public_key();
    use base64::{Engine, engine::general_purpose::STANDARD};
    Mock::given(path("/repos/owner/repo/actions/secrets/public-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"key_id":"test","key":STANDARD.encode(public.as_bytes())}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let mut c = connection();
    c.branch = "old-branch".into();
    let error = GithubClient::new("token".into(), &server.uri())
        .unwrap()
        .initialize(&c, "DOCKER_SYNC_KEY_TEST", &"02".repeat(32))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作流"));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method != "PUT")
    );
}

use docker_sync::{
    crypto::Crypto,
    domain::*,
    github::GithubClient,
    runner::digest,
    store::Store,
    web::{AppConfig, AppState, build_rocket},
};
use rocket::{
    http::{ContentType, Header, Status},
    local::asynchronous::Client,
};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    os::unix::fs::PermissionsExt,
    process::Command,
};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn post(client: &Client, url: &str, csrf: &str, value: Value) -> Value {
    let response = client
        .post(url)
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.to_owned()))
        .body(value.to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    response.into_json().await.unwrap()
}
#[rocket::async_test]
async fn browser_api_to_github_to_real_runner_and_back_handles_all_three_platforms() {
    let dir = tempfile::tempdir().unwrap();
    let github = MockServer::start().await;
    let crypto = Crypto::new([1; 32], [2; 32]);
    let store = Store::open(dir.path().join("test.db"), crypto).unwrap();
    let mut config = AppConfig::for_password("local-integration-password").unwrap();
    config.secure_cookie = false;
    config.worker_enabled = false;
    let client = Client::tracked(build_rocket(
        config,
        store.clone(),
        GithubClient::new("github-token".into(), &github.uri()).unwrap(),
    ))
    .await
    .unwrap();
    client.get("/login").dispatch().await;
    let session: Value = client
        .get("/api/session")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    post(
        &client,
        "/api/login",
        session["csrf"].as_str().unwrap(),
        json!({"password":"local-integration-password"}),
    )
    .await;
    let session: Value = client
        .get("/api/session")
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    let csrf = session["csrf"].as_str().unwrap();
    let settings = client
        .put("/api/settings")
        .header(ContentType::JSON)
        .header(Header::new("X-CSRF-Token", csrf.to_owned()))
        .body(
            json!({"owner":"owner","repo":"repo","branch":"main","token":"github-token"})
                .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(settings.status(), Status::Ok);
    use base64::{Engine, engine::general_purpose::STANDARD};
    let public = crypto_box::SecretKey::from_bytes([7; 32]).public_key();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/workflows/web-sync.yml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"state":"active","name":"Web 镜像同步 v2"})),
        )
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/secrets/public-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"key_id":"test","key":STANDARD.encode(public.as_bytes())})),
        )
        .mount(&github)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/repos/owner/repo/contents/.github/workflows/web-sync.yml",
        ))
        .and(wiremock::matchers::query_param("ref", "main"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(include_str!("../workflows/web-sync.yml")),
        )
        .mount(&github)
        .await;
    post(&client, "/api/settings/initialize", csrf, json!({})).await;
    assert!(store.settings().unwrap().unwrap().ready);
    let registry=post(&client,"/api/registries",csrf,json!({"name":"集成测试 ACR","provider":"aliyun_acr","endpoint":"registry.example.com","namespace":"demo","username":"user","password":"private-registry-password","enabled":true})).await;
    let mut ids = Vec::new();
    for (platform, tag) in [
        ("all", "all"),
        ("linux/amd64", "amd64"),
        ("linux/arm64", "arm64"),
    ] {
        let rule=post(&client,"/api/rules",csrf,json!({"name":platform,"source":"example:latest","registry_id":registry["id"],"target_repository":"example","target_tag":tag,"platform":platform,"enabled":true})).await;
        ids.push(rule["id"].clone());
    }
    Mock::given(method("POST")).and(path("/repos/owner/repo/actions/workflows/web-sync.yml/dispatches")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"workflow_run_id":42,"run_url":"https://api.github.com/runs/42","html_url":"https://github.com/owner/repo/actions/runs/42"}))).mount(&github).await;
    let task = post(
        &client,
        "/api/tasks",
        csrf,
        json!({"rule_ids":ids,"request_key":Uuid::new_v4()}),
    )
    .await;
    let task_id = Uuid::parse_str(task["id"].as_str().unwrap()).unwrap();
    let worker = &client.rocket().state::<AppState>().unwrap().worker;
    worker.tick().await.unwrap();
    let requests = github.received_requests().await.unwrap();
    let dispatched = requests
        .iter()
        .find(|r| r.url.path().ends_with("/dispatches"))
        .unwrap();
    let sent: Value = serde_json::from_slice(&dispatched.body).unwrap();
    assert_eq!(
        sent["inputs"]["secret_name"],
        store.crypto.task_secret_name()
    );
    let fixture = dir.path().join("fixtures");
    std::fs::create_dir(&fixture).unwrap();
    let amd = serde_json::to_vec(
        &json!({"schemaVersion":2,"config":{"architecture":"amd64"},"layers":[]}),
    )
    .unwrap();
    let arm = serde_json::to_vec(
        &json!({"schemaVersion":2,"config":{"architecture":"arm64"},"layers":[]}),
    )
    .unwrap();
    let ad = digest(&amd);
    let ar = digest(&arm);
    let root=serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[{"digest":ad,"platform":{"os":"linux","architecture":"amd64"}},{"digest":ar,"platform":{"os":"linux","architecture":"arm64"}}]})).unwrap();
    std::fs::write(fixture.join("root"), root).unwrap();
    std::fs::write(fixture.join("amd64"), amd).unwrap();
    std::fs::write(fixture.join("arm64"), arm).unwrap();
    let script = r#"#!/bin/sh
previous=""
for last do before="$previous"; previous="$last"; done
case "$*" in
  *"inspect --config"*) case "$last" in *"$ARM_DIGEST") printf '%s' '{"os":"linux","architecture":"arm64"}';; *) printf '%s' '{"os":"linux","architecture":"amd64"}';; esac ;;
  *"inspect --raw"*)
    case "$last" in
      *registry.example.com*) tag="${last##*:}"; if [ -f "$FAKE_FIXTURE/target-$tag" ]; then cat "$FAKE_FIXTURE/target-$tag"; else echo 'manifest unknown' >&2; exit 1; fi ;;
      *"$AMD_DIGEST") cat "$FAKE_FIXTURE/amd64";;
      *"$ARM_DIGEST") cat "$FAKE_FIXTURE/arm64";;
      *) cat "$FAKE_FIXTURE/root";;
    esac ;;
  *)
    tag="${last##*:}"
    case "$before" in *"$AMD_DIGEST") cp "$FAKE_FIXTURE/amd64" "$FAKE_FIXTURE/target-$tag";; *"$ARM_DIGEST") cp "$FAKE_FIXTURE/arm64" "$FAKE_FIXTURE/target-$tag";; *) cp "$FAKE_FIXTURE/root" "$FAKE_FIXTURE/target-$tag";; esac ;;
esac
"#;
    std::fs::write(fixture.join("skopeo"), script).unwrap();
    std::fs::set_permissions(
        fixture.join("skopeo"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let command = Command::new(env!("CARGO_BIN_EXE_sync-runner"))
        .args(["execute", "--output-dir"])
        .arg(dir.path().join("artifacts"))
        .env(
            "PATH",
            format!("{}:{}", fixture.display(), std::env::var("PATH").unwrap()),
        )
        .env("FAKE_FIXTURE", &fixture)
        .env("AMD_DIGEST", &ad)
        .env("ARM_DIGEST", &ar)
        .env("TASK_ID", task_id.to_string())
        .env("TASK_PROTOCOL_VERSION", "1")
        .env("TASK_PAYLOAD", sent["inputs"]["payload"].as_str().unwrap())
        .env("SYNC_PAYLOAD_KEY", hex::encode([2; 32]))
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env_remove("GITHUB_ACTIONS")
        .output()
        .unwrap();
    assert!(
        command.status.success(),
        "{}",
        String::from_utf8_lossy(&command.stdout)
    );
    let result = std::fs::read(dir.path().join("artifacts/result.json")).unwrap();
    let parsed: BatchResult = serde_json::from_slice(&result).unwrap();
    assert_eq!(parsed.summary.synced, 3);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("result.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&result).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/runs/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id":42,"status":"completed","conclusion":"success","run_attempt":1}),
        ))
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/runs/42/artifacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"artifacts":[{"id":9,"name":"image-sync-result-1","expired":false}]}),
        ))
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/artifacts/9/zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(&github)
        .await;
    store.poll_after(task_id, 0).unwrap();
    worker.tick().await.unwrap();
    let response = client.get(format!("/api/tasks/{task_id}")).dispatch().await;
    let detail: Value = response.into_json().await.unwrap();
    assert_eq!(detail["task"]["status"], "succeeded");
    assert_eq!(detail["result"]["summary"]["synced"], 3);
    assert!(!detail.to_string().contains("private-registry-password"));
}

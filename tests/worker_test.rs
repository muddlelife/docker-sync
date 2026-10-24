use docker_sync::{
    crypto::Crypto,
    domain::*,
    github::{GithubApi, GithubClient},
    store::{Store, now},
    worker::{Clock, Worker},
};
use std::sync::Arc;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> i64 {
        now() + 100
    }
}
fn setup() -> (tempfile::TempDir, Store, TaskView) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    store
        .save_settings(SettingsInput {
            owner: "owner".into(),
            repo: "repo".into(),
            branch: "main".into(),
            token: Some("token".into()),
        })
        .unwrap();
    let r = store
        .save_registry(
            None,
            RegistryInput {
                name: "ACR".into(),
                provider: "aliyun_acr".into(),
                endpoint: "registry.example.com".into(),
                namespace: "demo".into(),
                username: "u".into(),
                password: Some("private".into()),
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
                registry_id: r.id,
                target_repository: "nginx".into(),
                target_tag: "latest".into(),
                platform: Platform::All,
                enabled: true,
                source_username: None,
                source_password: None,
            },
        )
        .unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let task = store.enqueue(&[rule.id], Uuid::new_v4()).unwrap();
    (dir, store, task)
}
#[rocket::async_test]
async fn unknown_dispatch_never_reposts_and_restart_reconciles_exact_run() {
    let (_dir, store, task) = setup();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"workflow_runs":[]})),
        )
        .mount(&server)
        .await;
    let github: Arc<dyn GithubApi> =
        Arc::new(GithubClient::new("token".into(), &server.uri()).unwrap());
    let worker = Worker::new(store.clone(), github, Arc::new(FixedClock));
    worker.tick().await.unwrap();
    assert_eq!(
        store.task(task.id).unwrap().status,
        TaskStatus::DispatchUnknown
    );
    store.poll_after(task.id, 0).unwrap();
    worker.tick().await.unwrap();
    assert_eq!(
        store.task(task.id).unwrap().status,
        TaskStatus::DispatchUnknown
    );
    server.reset().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"workflow_runs":[{"id":42,"display_title":format!("image-sync-{}",task.id),"head_branch":"main","event":"workflow_dispatch","run_attempt":1}]}))).mount(&server).await;
    store.poll_after(task.id, 0).unwrap();
    worker.tick().await.unwrap();
    assert_eq!(store.task(task.id).unwrap().run_id, Some(42));
}
#[rocket::async_test]
async fn successful_run_without_result_is_not_successful_sync() {
    let (_dir, store, task) = setup();
    let server = MockServer::start().await;
    store.claim_next().unwrap();
    store
        .mutate(task.id, |v| {
            v.status = TaskStatus::Running;
            v.run_id = Some(42);
        })
        .unwrap();
    Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/42")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":42,"status":"completed","conclusion":"success","run_attempt":1}))).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/runs/42/artifacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"artifacts":[]})))
        .mount(&server)
        .await;
    let worker = Worker::new(
        store.clone(),
        Arc::new(GithubClient::new("token".into(), &server.uri()).unwrap()),
        Arc::new(FixedClock),
    );
    worker.tick().await.unwrap();
    assert_eq!(
        store.task(task.id).unwrap().status,
        TaskStatus::ResultPending
    );
}

struct FarClock;
impl Clock for FarClock {
    fn now(&self) -> i64 {
        now() + 90001
    }
}
fn zip_result(value: serde_json::Value) -> Vec<u8> {
    use std::io::{Cursor, Write};
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("result.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    writer.finish().unwrap().into_inner()
}
#[rocket::async_test]
async fn invalid_results_escalate_and_known_runs_never_reenter_dispatch_discovery() {
    for mismatched_id in [true, false] {
        let (_dir, store, task) = setup();
        let server = MockServer::start().await;
        store.claim_next().unwrap();
        store
            .mutate(task.id, |t| {
                t.status = TaskStatus::Running;
                t.run_id = Some(42)
            })
            .unwrap();
        Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/42")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":42,"status":"completed","conclusion":"success","run_attempt":1}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/42/artifacts")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"artifacts":[{"id":9,"name":"image-sync-result-1","expired":false}]}))).mount(&server).await;
        let id = if mismatched_id {
            Uuid::new_v4()
        } else {
            task.id
        };
        let bytes = zip_result(
            serde_json::json!({"version":1,"task_id":id,"run_attempt":1,"items":[],"summary":{"synced":0,"unchanged":0,"failed":0}}),
        );
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/actions/artifacts/9/zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        let worker = Worker::new(
            store.clone(),
            Arc::new(GithubClient::new("token".into(), &server.uri()).unwrap()),
            Arc::new(FarClock),
        );
        worker.tick().await.unwrap();
        assert_eq!(store.task(task.id).unwrap().status, TaskStatus::NeedsReview);
        store.poll_after(task.id, 0).unwrap();
        worker.tick().await.unwrap();
        assert_eq!(store.task(task.id).unwrap().status, TaskStatus::NeedsReview);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|r| !r.url.path().contains("/workflows/"))
        );
        store.release_unknown(task.id).unwrap();
        assert_eq!(store.task(task.id).unwrap().status, TaskStatus::Cancelled);
    }
}
#[rocket::async_test]
async fn reconciliation_and_artifact_rate_limits_block_requests_until_deadline() {
    for reconcile in [true, false] {
        let (_dir, store, task) = setup();
        let server = MockServer::start().await;
        store.claim_next().unwrap();
        store
            .mutate(task.id, |t| {
                t.status = if reconcile {
                    TaskStatus::DispatchUnknown
                } else {
                    TaskStatus::Running
                };
                t.run_id = if reconcile { None } else { Some(42) };
            })
            .unwrap();
        if !reconcile {
            Mock::given(method("GET")).and(path("/repos/owner/repo/actions/runs/42")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":42,"status":"completed","conclusion":"success","run_attempt":1}))).with_priority(1).mount(&server).await;
        }
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
            .with_priority(2)
            .mount(&server)
            .await;
        let worker = Worker::new(
            store.clone(),
            Arc::new(GithubClient::new("token".into(), &server.uri()).unwrap()),
            Arc::new(FixedClock),
        );
        worker.tick().await.unwrap();
        assert!(store.record(task.id).unwrap().next_poll_at >= now() + 215);
        let before = server.received_requests().await.unwrap().len();
        worker.tick().await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), before);
    }
}

#[rocket::async_test]
async fn dispatch_rate_limit_does_not_skip_to_another_pending_batch() {
    let (_dir, store, task) = setup();
    let server = MockServer::start().await;
    let existing = store.rules().unwrap().remove(0);
    let second = store
        .save_rule(
            None,
            RuleInput {
                name: "second".into(),
                source: existing.source,
                registry_id: existing.registry_id,
                target_repository: existing.target_repository,
                target_tag: "second".into(),
                platform: Platform::All,
                enabled: true,
                source_username: None,
                source_password: None,
            },
        )
        .unwrap();
    let second_task = store.enqueue(&[second.id], Uuid::new_v4()).unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
        .mount(&server)
        .await;
    let worker = Worker::new(
        store.clone(),
        Arc::new(GithubClient::new("token".into(), &server.uri()).unwrap()),
        Arc::new(FixedClock),
    );
    worker.tick().await.unwrap();
    worker.tick().await.unwrap();
    assert_eq!(store.task(task.id).unwrap().status, TaskStatus::Pending);
    assert_eq!(
        store.task(second_task.id).unwrap().status,
        TaskStatus::Pending
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

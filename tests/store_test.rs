use docker_sync::{crypto::Crypto, domain::*, store::Store};
use tempfile::TempDir;
use uuid::Uuid;

fn db() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    (dir, store)
}
fn registry(endpoint: &str) -> RegistryInput {
    RegistryInput {
        name: "阿里云".into(),
        endpoint: endpoint.into(),
        namespace: "demo".into(),
        username: "user".into(),
        password: Some("private-pass".into()),
        enabled: true,
        provider: "aliyun_acr".into(),
    }
}
fn rule(id: Uuid, target: &str) -> RuleInput {
    RuleInput {
        name: "Nginx".into(),
        source: "nginx:latest".into(),
        registry_id: id,
        target_repository: "nginx".into(),
        target_tag: target.into(),
        platform: Platform::All,
        enabled: true,
        source_username: None,
        source_password: None,
    }
}
fn settings() -> SettingsInput {
    SettingsInput {
        owner: "owner".into(),
        repo: "repo".into(),
        branch: "main".into(),
        token: Some("github-private".into()),
    }
}

#[test]
fn encrypted_configuration_survives_reopen_and_blank_password_preserves_it() {
    let (dir, store) = db();
    let r = store
        .save_registry(None, registry("REGISTRY.example.com"))
        .unwrap();
    let mut update = registry("registry.example.com");
    update.password = None;
    store.save_registry(Some(r.id), update).unwrap();
    drop(store);
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    assert_eq!(
        store.registries().unwrap()[0].endpoint,
        "registry.example.com"
    );
    assert!(store.registries().unwrap()[0].has_password);
    assert!(
        !serde_json::to_string(&store.registries().unwrap())
            .unwrap()
            .contains("private-pass")
    );
    let bytes = std::fs::read(dir.path().join("test.db")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("private-pass"));
}

#[test]
fn full_target_uniqueness_spans_registry_ids_and_namespace_changes_roll_back() {
    let (_dir, store) = db();
    let a = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    let b = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    store.save_rule(None, rule(a.id, "latest")).unwrap();
    assert!(store.save_rule(None, rule(b.id, "latest")).is_err());
    assert!(store.delete_registry(a.id).is_err());
    let mut separate = registry("registry.example.com");
    separate.namespace = "other".into();
    store.save_registry(Some(b.id), separate).unwrap();
    store.save_rule(None, rule(b.id, "latest")).unwrap();
    assert!(
        store
            .save_registry(Some(b.id), registry("registry.example.com"))
            .is_err()
    );
    assert_eq!(
        store
            .registries()
            .unwrap()
            .iter()
            .find(|r| r.id == b.id)
            .unwrap()
            .namespace,
        "other"
    );
}

#[test]
fn queue_deduplicates_requests_claims_once_and_freezes_configuration() {
    let (_dir, store) = db();
    store.save_settings(settings()).unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let registry = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    let rule = store.save_rule(None, rule(registry.id, "latest")).unwrap();
    let request = Uuid::new_v4();
    let task = store.enqueue(&[rule.id], request).unwrap();
    assert_eq!(store.enqueue(&[rule.id], request).unwrap().id, task.id);
    assert!(store.enqueue(&[rule.id], Uuid::new_v4()).is_err());
    let claimed = store.claim_next().unwrap().unwrap();
    assert_eq!(claimed.id, task.id);
    assert!(store.claim_next().unwrap().is_none());
    assert_eq!(
        store.payload(task.id).unwrap().items[0].source,
        "docker.io/library/nginx:latest"
    );
    let mut new_settings = settings();
    new_settings.repo = "different".into();
    assert!(store.save_settings(new_settings).is_err());
    store.recover_dispatching().unwrap();
    assert_eq!(
        store.task(task.id).unwrap().status,
        TaskStatus::DispatchUnknown
    );
}

#[test]
fn task_duration_uses_current_attempt_results_including_failed_items() {
    let (dir, store) = db();
    store.save_settings(settings()).unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let registry = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    let first = store.save_rule(None, rule(registry.id, "first")).unwrap();
    let second = store.save_rule(None, rule(registry.id, "second")).unwrap();
    let task = store
        .enqueue(&[first.id, second.id], Uuid::new_v4())
        .unwrap();
    assert!(serde_json::to_value(&task).unwrap()["elapsed_ms"].is_null());
    // 排队、轮询及结果下载时间不应混入实际执行耗时。
    store.mutate(task.id, |t| t.created_at = 1).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let items = task
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| ItemResult {
            item_id: item.item_id,
            source: item.source.clone(),
            target: item.target.clone(),
            platform: item.platform,
            resolved_platform: Some("all".into()),
            source_index_digest: Some(digest.clone()),
            source_digest: Some(digest.clone()),
            target_digest: (index == 0).then(|| digest.clone()),
            outcome: if index == 0 {
                ItemOutcome::Synced
            } else {
                ItemOutcome::Failed
            },
            error: (index == 1).then(|| "fixture failure".into()),
            elapsed_ms: if index == 0 { 1350 } else { 1500 },
        })
        .collect();
    let mut result = BatchResult {
        version: 1,
        task_id: task.id,
        run_attempt: 1,
        items,
        summary: Summary {
            synced: 1,
            unchanged: 0,
            failed: 1,
        },
    };
    store.apply_result(&result).unwrap();
    let rules = serde_json::to_value(store.rules().unwrap()).unwrap();
    let a = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == first.id.to_string())
        .unwrap();
    let b = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == second.id.to_string())
        .unwrap();
    assert_eq!(a["sync"]["status"], "succeeded");
    assert_eq!(a["sync"]["elapsed_ms"], 1350);
    assert_eq!(b["sync"]["status"], "failed");
    assert_eq!(b["sync"]["error"], "fixture failure");
    assert_eq!(a["sync"]["task_id"], task.id.to_string());
    assert_eq!(
        serde_json::to_value(store.task(task.id).unwrap()).unwrap()["elapsed_ms"],
        2850
    );
    assert_eq!(
        serde_json::to_value(store.tasks().unwrap()).unwrap()[0]["elapsed_ms"],
        2850
    );
    drop(store);
    let store = Store::open(dir.path().join("test.db"), Crypto::new([1; 32], [2; 32])).unwrap();
    assert_eq!(
        serde_json::to_value(store.task(task.id).unwrap()).unwrap()["elapsed_ms"],
        2850
    );
    store.reopen_attempt(task.id, 2).unwrap();
    let rules = serde_json::to_value(store.rules().unwrap()).unwrap();
    assert!(
        rules
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["sync"]["status"] == "queued" && r["sync"]["elapsed_ms"].is_null())
    );
    assert!(serde_json::to_value(store.task(task.id).unwrap()).unwrap()["elapsed_ms"].is_null());
    assert!(serde_json::to_value(store.tasks().unwrap()).unwrap()[0]["elapsed_ms"].is_null());
    result.run_attempt = 2;
    result.items[0].elapsed_ms = 0;
    result.items[1].elapsed_ms = 0;
    store.apply_result(&result).unwrap();
    assert_eq!(
        serde_json::to_value(store.task(task.id).unwrap()).unwrap()["elapsed_ms"],
        0
    );
    assert_eq!(
        store.result(task.id, 1).unwrap().unwrap().items[0].elapsed_ms,
        1350
    );
    let retry = store.retry(task.id).unwrap();
    assert!(serde_json::to_value(&retry).unwrap()["elapsed_ms"].is_null());
}

#[test]
fn rule_status_ignores_other_configurations_and_searches_beyond_latest_200_tasks() {
    let (_dir, store) = db();
    store.save_settings(settings()).unwrap();
    let (_, revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&revision).unwrap();
    let reg = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    let first = store.save_rule(None, rule(reg.id, "first")).unwrap();
    let never = serde_json::to_value(store.rules().unwrap()).unwrap();
    assert_eq!(never[0]["sync"]["status"], "never");
    let task = store.enqueue(&[first.id], Uuid::new_v4()).unwrap();
    store
        .transition(
            task.id,
            TaskStatus::DispatchFailed,
            Some("permission denied".into()),
        )
        .unwrap();
    let second = store.save_rule(None, rule(reg.id, "second")).unwrap();
    for _ in 0..201 {
        let task = store.enqueue(&[second.id], Uuid::new_v4()).unwrap();
        store
            .transition(task.id, TaskStatus::Cancelled, None)
            .unwrap();
    }
    let history = store.task_page(1, 10).unwrap();
    assert_eq!(history.total, 202);
    assert_eq!(history.pages, 21);
    assert_eq!(history.items.len(), 10);
    let last = store.task_page(999, 10).unwrap();
    assert_eq!(last.page, 21);
    assert_eq!(last.items.len(), 2);
    assert_eq!(last.items[1].id, task.id);
    let rows = serde_json::to_value(store.rules().unwrap()).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == first.id.to_string())
        .unwrap();
    assert_eq!(row["sync"]["status"], "dispatch_failed");
    assert_eq!(row["sync"]["error"], "permission denied");
    let mut changed = rule(reg.id, "first");
    changed.platform = Platform::LinuxArm64;
    store.save_rule(Some(first.id), changed).unwrap();
    let rows = serde_json::to_value(store.rules().unwrap()).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == first.id.to_string())
        .unwrap();
    assert_eq!(row["sync"]["status"], "never");
}

#[test]
fn changing_github_settings_invalidates_initialization_and_blocks_unready_tasks() {
    let (_dir, store) = db();
    let view = store.save_settings(settings()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap()["ready"], false);
    let r = store
        .save_registry(None, registry("registry.example.com"))
        .unwrap();
    let rule = store.save_rule(None, rule(r.id, "latest")).unwrap();
    assert!(store.enqueue(&[rule.id], Uuid::new_v4()).is_err());
    let (_, old_revision, _) = store.connection_setup().unwrap();
    store.mark_settings_ready(&old_revision).unwrap();
    assert!(store.settings().unwrap().unwrap().ready);
    store.save_settings(settings()).unwrap();
    assert!(!store.settings().unwrap().unwrap().ready);
    assert!(store.mark_settings_ready(&old_revision).is_err());
}

#[test]
fn task_pagination_handles_empty_history_and_invalid_sizes() {
    let (_dir, store) = db();
    let page = store.task_page(1, 10).unwrap();
    assert_eq!(page.total, 0);
    assert_eq!(page.page, 1);
    assert_eq!(page.pages, 1);
    assert!(page.items.is_empty());
    assert!(store.task_page(0, 10).is_err());
    assert!(store.task_page(1, 0).is_err());
    assert!(store.task_page(1, 101).is_err());
}

use docker_sync::{crypto::Crypto, domain::*};
use std::{os::unix::fs::PermissionsExt, process::Command};
use uuid::Uuid;

#[test]
fn runner_executes_encrypted_workflow_input_and_writes_only_public_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("skopeo");
    let script = r#"#!/bin/sh
if [ "$1" = "inspect" ]; then
  for last do :; done
  case "$last" in
    *registry.example.com*) if [ ! -f "$FAKE_STATE" ]; then echo "manifest unknown" >&2; exit 1; fi ;;
  esac
  printf '%s' '{"schemaVersion":2,"config":{},"layers":[]}'
else
  touch "$FAKE_STATE"
fi
"#;
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let task_id = Uuid::new_v4();
    let payload = TaskPayload {
        version: 1,
        task_id,
        items: vec![SyncItem {
            item_id: Uuid::new_v4(),
            source: "docker.io/library/nginx:latest".into(),
            target: "registry.example.com/demo/nginx:latest".into(),
            platform: Platform::All,
            source_credentials: None,
            target_credentials: Credentials::new("user".into(), "private-runner-password".into()),
        }],
    };
    let crypto = Crypto::new([1; 32], [2; 32]);
    let envelope = crypto.seal_task(&payload).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sync-runner"))
        .args(["execute", "--output-dir"])
        .arg(dir.path().join("artifacts"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("FAKE_STATE", dir.path().join("state"))
        .env("TASK_ID", task_id.to_string())
        .env("TASK_PROTOCOL_VERSION", "1")
        .env("TASK_PAYLOAD", envelope)
        .env("SYNC_PAYLOAD_KEY", hex::encode([2; 32]))
        .env("GITHUB_RUN_ATTEMPT", "3")
        .env_remove("GITHUB_ACTIONS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-runner-password"));
    let result = std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
    assert!(!result.contains("private-runner-password"));
    let result: BatchResult = serde_json::from_str(&result).unwrap();
    assert_eq!(result.task_id, task_id);
    assert_eq!(result.run_attempt, 3);
    assert_eq!(result.summary.synced, 1);
    assert!(
        !std::fs::read_to_string(dir.path().join("artifacts/sync.log"))
            .unwrap()
            .contains("private-runner-password")
    );
}

#[test]
fn old_list_command_is_not_a_supported_entrypoint() {
    let output = Command::new(env!("CARGO_BIN_EXE_sync-runner"))
        .args(["legacy", "--images", "missing.txt"])
        .env_remove("ACR_REGISTRY")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn missing_or_invalid_task_secret_reports_setup_instructions_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    for value in [
        None,
        Some(""),
        Some("abcd"),
        Some("github_pat_example-do-not-use"),
        Some("gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg"),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sync-runner"));
        command
            .args(["execute", "--output-dir"])
            .arg(dir.path().join("artifacts"))
            .env("TASK_PROTOCOL_VERSION", "1")
            .env_remove("SYNC_PAYLOAD_KEY");
        if let Some(value) = value {
            command.env("SYNC_PAYLOAD_KEY", value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.contains("SYNC_PAYLOAD_KEY")
                && message.contains("Secret")
                && message.contains("64"),
            "{message}"
        );
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            assert!(!message.contains(value));
        }
        assert!(!dir.path().join("artifacts").exists());
    }
}

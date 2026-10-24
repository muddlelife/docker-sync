use docker_sync::{crypto::Crypto, domain::*, reference::ImageReference};
use uuid::Uuid;

#[test]
fn references_normalize_without_splitting_registry_ports() {
    for (input, expected) in [
        ("nginx", "docker.io/library/nginx:latest"),
        ("nginx:1.27", "docker.io/library/nginx:1.27"),
        ("org/image:dev", "docker.io/org/image:dev"),
        ("localhost:5000/a/b:dev", "localhost:5000/a/b:dev"),
        (
            "registry.example.com:443/a:dev",
            "registry.example.com/a:dev",
        ),
    ] {
        assert_eq!(ImageReference::parse(input).unwrap().canonical(), expected);
    }
    for input in [
        "dir:/tmp/a",
        "docker://nginx",
        "a\nb",
        "user:pass@host/a",
        "-bad",
        "a:",
    ] {
        assert!(ImageReference::parse(input).is_err(), "{input}");
    }
    let digest = format!("ghcr.io/org/name@sha256:{}", "a".repeat(64));
    assert_eq!(ImageReference::parse(&digest).unwrap().canonical(), digest);
}

#[test]
fn authenticated_envelopes_bind_task_identity_and_hide_credentials() {
    let crypto = Crypto::new([7; 32], [8; 32]);
    let id = Uuid::new_v4();
    let payload = TaskPayload {
        version: 1,
        task_id: id,
        items: vec![SyncItem {
            item_id: Uuid::new_v4(),
            source: "nginx:latest".into(),
            target: "registry.example.com/ns/nginx:latest".into(),
            platform: Platform::All,
            source_credentials: None,
            target_credentials: Credentials::new("user".into(), "super-secret".into()),
        }],
    };
    let envelope = crypto.seal_task(&payload).unwrap();
    assert!(!envelope.contains("super-secret"));
    assert_ne!(envelope, crypto.seal_task(&payload).unwrap());
    assert_eq!(crypto.open_task(id, &envelope).unwrap().task_id, id);
    assert!(crypto.open_task(Uuid::new_v4(), &envelope).is_err());
    assert!(
        Crypto::new([7; 32], [9; 32])
            .open_task(id, &envelope)
            .is_err()
    );
    assert!(!format!("{:?}", payload).contains("super-secret"));
    let mut broken = envelope.into_bytes();
    broken[20] = if broken[20] == b'A' { b'B' } else { b'A' };
    assert!(
        crypto
            .open_task(id, std::str::from_utf8(&broken).unwrap())
            .is_err()
    );
}

#[test]
fn payload_limits_reject_empty_oversized_or_invalid_platforms() {
    assert!(serde_json::from_str::<Platform>("\"linux/arm\"").is_err());
    assert_eq!(
        serde_json::from_str::<Platform>("\"linux/arm64\"").unwrap(),
        Platform::LinuxArm64
    );
    let payload = TaskPayload {
        version: 1,
        task_id: Uuid::new_v4(),
        items: vec![],
    };
    assert!(payload.validate().is_err());
}

#[test]
fn batch_item_and_encrypted_input_limits_are_checked_before_dispatch() {
    let mut payload = TaskPayload {
        version: 1,
        task_id: Uuid::new_v4(),
        items: (0..20)
            .map(|n| SyncItem {
                item_id: Uuid::new_v4(),
                source: "nginx:latest".into(),
                target: format!("registry.example.com/ns/nginx-{n}:latest"),
                platform: Platform::All,
                source_credentials: None,
                target_credentials: Credentials::new("user".into(), "password".into()),
            })
            .collect(),
    };
    let crypto = Crypto::new([1; 32], [2; 32]);
    assert!(crypto.seal_task(&payload).is_ok());
    let mut extra = payload.items[0].clone();
    extra.item_id = Uuid::new_v4();
    extra.target = "registry.example.com/ns/extra:latest".into();
    payload.items.push(extra);
    assert!(payload.validate().is_err());
    payload.items.pop();
    for item in &mut payload.items {
        item.target_credentials.password = "p".repeat(4096);
    }
    assert!(payload.validate().is_ok());
    assert!(crypto.seal_task(&payload).is_err());
}

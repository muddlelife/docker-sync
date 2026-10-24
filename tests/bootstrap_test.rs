use docker_sync::{bootstrap::Bootstrap, crypto::Crypto};

#[test]
fn first_start_generates_persistent_independent_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let (first, created) = Bootstrap::load_or_create(dir.path()).unwrap();
    assert!(created);
    assert!(first.admin_password.len() >= 24);
    assert_ne!(first.app_encryption_key, first.sync_payload_key);
    Crypto::key_from_hex(&first.app_encryption_key).unwrap();
    Crypto::key_from_hex(&first.sync_payload_key).unwrap();
    let (again, created) = Bootstrap::load_or_create(dir.path()).unwrap();
    assert!(!created);
    assert_eq!(first.admin_password, again.admin_password);
    assert_eq!(first.app_encryption_key, again.app_encryption_key);
    assert_eq!(first.sync_payload_key, again.sync_payload_key);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("bootstrap.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn corrupt_or_missing_credentials_never_silently_replace_existing_keys() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bootstrap.json"), "broken").unwrap();
    assert!(Bootstrap::load_or_create(dir.path()).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("bootstrap.json")).unwrap(),
        "broken"
    );
    std::fs::remove_file(dir.path().join("bootstrap.json")).unwrap();
    std::fs::write(dir.path().join("docker-sync.db"), "existing database").unwrap();
    assert!(Bootstrap::load_or_create(dir.path()).is_err());
    assert!(!dir.path().join("bootstrap.json").exists());
}

#[test]
fn resetting_password_keeps_both_encryption_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (before, _) = Bootstrap::load_or_create(dir.path()).unwrap();
    let after = Bootstrap::reset_password(dir.path()).unwrap();
    assert_ne!(before.admin_password, after.admin_password);
    assert_eq!(before.app_encryption_key, after.app_encryption_key);
    assert_eq!(before.sync_payload_key, after.sync_payload_key);
    assert_eq!(
        Bootstrap::load_or_create(dir.path())
            .unwrap()
            .0
            .admin_password,
        after.admin_password
    );
}

use docker_sync::{
    domain::*,
    error::{Error, Result},
    runner::{ImageTransport, digest, run_batch},
};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};
use uuid::Uuid;

struct Fake {
    reads: Cell<usize>,
    raw: RefCell<HashMap<String, Vec<u8>>>,
    configs: HashMap<String, serde_json::Value>,
    copies: RefCell<Vec<(String, Platform)>>,
}
impl ImageTransport for Fake {
    fn raw_manifest(&self, r: &str, _: Option<&Credentials>) -> Result<Vec<u8>> {
        self.reads.set(self.reads.get() + 1);
        self.raw
            .borrow()
            .get(r)
            .cloned()
            .ok_or_else(|| Error::NotFound("manifest unknown".into()))
    }
    fn image_config(&self, r: &str, _: Option<&Credentials>) -> Result<serde_json::Value> {
        self.configs
            .get(r)
            .cloned()
            .ok_or_else(|| Error::invalid("missing config"))
    }
    fn copy(
        &self,
        source: &str,
        target: &str,
        p: Platform,
        _: Option<&Credentials>,
        _: &Credentials,
    ) -> Result<()> {
        self.copies.borrow_mut().push((source.into(), p));
        let bytes = self
            .raw
            .borrow()
            .get(source)
            .cloned()
            .ok_or_else(|| Error::invalid("source not pinned"))?;
        self.raw.borrow_mut().insert(target.into(), bytes);
        Ok(())
    }
}
fn fixture(platform: Platform) -> (TaskPayload, Fake, String, String) {
    let amd = serde_json::to_vec(&json!({"schemaVersion":2,"config":{"digest":"amd"},"layers":[]}))
        .unwrap();
    let arm = serde_json::to_vec(&json!({"schemaVersion":2,"config":{"digest":"arm"},"layers":[]}))
        .unwrap();
    let ad = digest(&amd);
    let rd = digest(&arm);
    let index = serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[
        {"digest":ad,"platform":{"os":"linux","architecture":"amd64"}},
        {"digest":rd,"platform":{"os":"linux","architecture":"arm64","variant":"v8"}}]}))
    .unwrap();
    let root = digest(&index);
    let repo = "docker.io/library/example";
    let mut raw = HashMap::new();
    raw.insert(format!("{repo}:latest"), index.clone());
    raw.insert(format!("{repo}@{root}"), index);
    raw.insert(format!("{repo}@{ad}"), amd);
    raw.insert(format!("{repo}@{rd}"), arm);
    let configs = HashMap::from([
        (
            format!("{repo}@{ad}"),
            json!({"os":"linux","architecture":"amd64"}),
        ),
        (
            format!("{repo}@{rd}"),
            json!({"os":"linux","architecture":"arm64","variant":"v8"}),
        ),
    ]);
    let payload = TaskPayload {
        version: 1,
        task_id: Uuid::new_v4(),
        items: vec![SyncItem {
            item_id: Uuid::new_v4(),
            source: format!("{repo}:latest"),
            target: "registry.example.com/ns/example:latest".into(),
            platform,
            source_credentials: None,
            target_credentials: Credentials::new("u".into(), "sensitive".into()),
        }],
    };
    (
        payload,
        Fake {
            reads: Cell::new(0),
            raw: RefCell::new(raw),
            configs,
            copies: RefCell::new(vec![]),
        },
        ad,
        rd,
    )
}

#[test]
fn platform_traversal_reserves_network_budget_before_fetching_children() {
    let (payload, mut fake, _, _) = fixture(Platform::LinuxAmd64);
    let mut entries = Vec::new();
    for n in 0..80 {
        let raw =
            serde_json::to_vec(&json!({"schemaVersion":2,"config":{"n":n},"layers":[]})).unwrap();
        let d = digest(&raw);
        let reference = format!("docker.io/library/example@{d}");
        fake.raw.borrow_mut().insert(reference.clone(), raw);
        fake.configs
            .insert(reference, json!({"os":"linux","architecture":"amd64"}));
        entries.push(json!({"digest":d,"platform":{"os":"linux","architecture":"amd64"}}));
    }
    fake.raw.borrow_mut().insert(
        payload.items[0].source.clone(),
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":entries})).unwrap(),
    );
    let result = run_batch(payload, &fake, 1);
    assert_eq!(result.summary.failed, 1);
    assert!(
        fake.reads.get() <= 64,
        "network reads: {}",
        fake.reads.get()
    );
}

#[test]
fn duplicate_descriptors_are_fetched_once() {
    let (payload, fake, amd, _) = fixture(Platform::LinuxAmd64);
    let descriptor = json!({"digest":amd,"platform":{"os":"linux","architecture":"amd64"}});
    fake.raw.borrow_mut().insert(
        payload.items[0].source.clone(),
        serde_json::to_vec(&json!({"schemaVersion":2,"manifests":vec![descriptor;100]})).unwrap(),
    );
    let resolved = docker_sync::runner::resolve_source(
        &fake,
        &payload.items[0].source,
        Platform::LinuxAmd64,
        None,
    )
    .unwrap();
    assert_eq!(resolved.copy_digest, amd);
    assert_eq!(fake.reads.get(), 2);
}
#[test]
fn single_platform_copies_verified_leaf_not_runner_or_index() {
    for platform in [Platform::LinuxAmd64, Platform::LinuxArm64] {
        let (p, f, amd, arm) = fixture(platform);
        let r = run_batch(p, &f, 1);
        assert_eq!(r.summary.synced, 1);
        let expected = if platform == Platform::LinuxAmd64 {
            amd
        } else {
            arm
        };
        assert_eq!(r.items[0].target_digest.as_deref(), Some(expected.as_str()));
        assert!(f.copies.borrow()[0].0.ends_with(&expected));
        assert_eq!(f.copies.borrow()[0].1, platform);
    }
}
#[test]
fn all_preserves_index_and_second_identical_sync_skips() {
    let (p, f, _, _) = fixture(Platform::All);
    let first = run_batch(p.clone(), &f, 1);
    assert_eq!(first.summary.synced, 1);
    assert_eq!(
        first.items[0].source_index_digest,
        first.items[0].target_digest
    );
    let second = run_batch(p, &f, 1);
    assert_eq!(second.summary.unchanged, 1);
    assert_eq!(f.copies.borrow().len(), 1);
}
#[test]
fn missing_platform_fails_without_fallback_and_batch_continues() {
    let (mut p, mut f, amd, arm) = fixture(Platform::LinuxArm64);
    f.configs
        .get_mut(&format!("docker.io/library/example@{arm}"))
        .unwrap()["architecture"] = json!("amd64");
    let mut second = p.items[0].clone();
    second.item_id = Uuid::new_v4();
    second.platform = Platform::LinuxAmd64;
    second.target = "registry.example.com/ns/example:amd64".into();
    p.items.push(second);
    let r = run_batch(p, &f, 1);
    assert_eq!(r.summary.failed, 1);
    assert_eq!(r.summary.synced, 1);
    assert_eq!(r.items[1].source_digest.as_deref(), Some(amd.as_str()));
    assert!(!serde_json::to_string(&r).unwrap().contains("sensitive"));
}

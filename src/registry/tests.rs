use super::*;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};
fn registry() -> RegistryView {
    RegistryView {
        id: uuid::Uuid::new_v4(),
        name: "测试 ACR".into(),
        provider: "aliyun_acr".into(),
        endpoint: "registry.example.com".into(),
        namespace: "demo".into(),
        username: "user".into(),
        has_password: true,
        enabled: true,
    }
}
fn client(server: &MockServer) -> RegistryClient {
    RegistryClient::with_base(
        Url::parse(&server.uri()).unwrap(),
        Credentials::new("user".into(), "private-password".into()),
    )
    .unwrap()
}
async fn tags(server: &MockServer, repository: &str) {
    Mock::given(path(format!("/v2/demo/{repository}/tags/list")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"name":format!("demo/{repository}"),"tags":["latest"]})),
        )
        .mount(server)
        .await;
}
fn index() -> serde_json::Value {
    json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[
        {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":format!("sha256:{}","a".repeat(64)),"size":200,"platform":{"os":"linux","architecture":"amd64"}},
        {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":format!("sha256:{}","b".repeat(64)),"size":200,"platform":{"os":"linux","architecture":"arm64","variant":"v8"}},
        {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":format!("sha256:{}","c".repeat(64)),"size":200,"platform":{"os":"unknown","architecture":"unknown"},"annotations":{"vnd.docker.reference.type":"attestation-manifest"}}
    ]})
}
#[rocket::async_test]
async fn catalog_pagination_limits_namespace_and_reads_real_multi_architecture_tags() {
    let server = MockServer::start().await;
    Mock::given(path("/v2/_catalog"))
        .and(query_param("last", "demo/nginx"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"repositories":["other/private","demo/redis"]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/v2/_catalog"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", "</v2/_catalog?n=100&last=demo/nginx>; rel=\"next\"")
                .set_body_json(json!({"repositories":["demo/nginx"]})),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    for repo in ["nginx", "redis"] {
        tags(&server, repo).await;
    }
    Mock::given(path("/v2/demo/nginx/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index()))
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/redis/manifests/latest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let result = client(&server)
        .browse(&registry(), &[], None)
        .await
        .unwrap();
    assert_eq!(result.scope, "catalog");
    assert_eq!(result.repositories, vec!["nginx", "redis"]);
    let image = result
        .images
        .iter()
        .find(|i| i.repository == "nginx")
        .unwrap();
    assert_eq!(image.platforms, vec!["linux/amd64", "linux/arm64/v8"]);
    assert!(image.digest.as_ref().unwrap().starts_with("sha256:"));
    assert!(
        result
            .images
            .iter()
            .find(|i| i.repository == "redis")
            .unwrap()
            .error
            .is_some()
    );
    assert!(!result.complete);
}
#[rocket::async_test]
async fn unsupported_catalog_falls_back_to_known_repositories_with_explicit_scope() {
    let server = MockServer::start().await;
    Mock::given(path("/v2/_catalog"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    tags(&server, "nginx").await;
    Mock::given(path("/v2/demo/nginx/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index()))
        .mount(&server)
        .await;
    let result = client(&server)
        .browse(&registry(), &["nginx".into()], None)
        .await
        .unwrap();
    assert_eq!(result.scope, "known");
    assert!(!result.complete);
    assert!(!result.warnings.is_empty());
    assert_eq!(result.images.len(), 1);
}
#[rocket::async_test]
async fn bearer_auth_uses_read_only_scope_and_single_arch_comes_from_config() {
    let server = MockServer::start().await;
    let digest = format!("sha256:{}", "d".repeat(64));
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .and(header("authorization", "Bearer fixture-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name":"demo/nginx","tags":["v1"]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/tags/list")).respond_with(ResponseTemplate::new(401)
        .insert_header("WWW-Authenticate",format!("Bearer realm=\"{}/token\",service=\"registry\",scope=\"repository:demo/nginx:pull,push\"",server.uri())))
        .with_priority(2).mount(&server).await;
    Mock::given(path("/token"))
        .and(query_param("scope", "repository:demo/nginx:pull"))
        .and(header(
            "authorization",
            "Basic dXNlcjpwcml2YXRlLXBhc3N3b3Jk",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"token":"fixture-token","expires_in":300})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/manifests/v1")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"digest":digest,"size":80},"layers":[]}))).mount(&server).await;
    Mock::given(path(format!("/v2/demo/nginx/blobs/{digest}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"os":"linux","architecture":"arm64"})),
        )
        .mount(&server)
        .await;
    let result = client(&server)
        .browse(&registry(), &[], Some("nginx"))
        .await
        .unwrap();
    assert_eq!(result.scope, "repository");
    assert!(result.complete);
    assert_eq!(result.images[0].platforms, vec!["linux/arm64"]);
}
#[rocket::async_test]
async fn hostile_auth_realm_and_pagination_never_receive_credentials() {
    let server = MockServer::start().await;
    let attacker = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token":"stolen"})))
        .mount(&attacker)
        .await;
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .respond_with(ResponseTemplate::new(401).insert_header(
            "WWW-Authenticate",
            format!(
                "Bearer realm=\"{}/token\",service=\"registry\"",
                attacker.uri()
            ),
        ))
        .mount(&server)
        .await;
    let result = client(&server)
        .browse(&registry(), &[], Some("nginx"))
        .await;
    assert!(result.is_err());
    assert!(attacker.received_requests().await.unwrap().is_empty());
    server.reset().await;
    Mock::given(path("/v2/_catalog"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Link", format!("<{}/steal>; rel=\"next\"", attacker.uri()))
                .set_body_json(json!({"repositories":[]})),
        )
        .mount(&server)
        .await;
    let result = client(&server).browse(&registry(), &[], None).await;
    assert!(result.is_err());
    assert!(attacker.received_requests().await.unwrap().is_empty());
}
#[rocket::async_test]
async fn unauthorized_and_missing_repository_are_errors_not_empty_success() {
    for status in [401, 404, 500] {
        let server = MockServer::start().await;
        Mock::given(path("/v2/demo/missing/tags/list"))
            .respond_with(ResponseTemplate::new(status).set_body_string("PRIVATE-UPSTREAM-TEXT"))
            .mount(&server)
            .await;
        let result = client(&server)
            .browse(&registry(), &[], Some("missing"))
            .await;
        let message = result.unwrap_err().to_string();
        assert!(!message.contains("PRIVATE-UPSTREAM-TEXT"));
    }
}
#[rocket::async_test]
async fn tag_pagination_is_followed_and_invalid_repository_never_makes_a_request() {
    let server = MockServer::start().await;
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .and(query_param("last", "one"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name":"demo/nginx","tags":["two"]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    "</v2/demo/nginx/tags/list?n=100&last=one>; rel=\"next\"",
                )
                .set_body_json(json!({"name":"demo/nginx","tags":["one"]})),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    for tag in ["one", "two"] {
        Mock::given(path(format!("/v2/demo/nginx/manifests/{tag}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(index()))
            .mount(&server)
            .await;
    }
    assert_eq!(
        client(&server)
            .browse(&registry(), &[], Some("nginx"))
            .await
            .unwrap()
            .images
            .len(),
        2
    );
    server.reset().await;
    assert!(
        client(&server)
            .browse(&registry(), &[], Some("../outside"))
            .await
            .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[rocket::async_test]
async fn catalog_token_denial_still_queries_readable_known_repository() {
    let server = MockServer::start().await;
    let challenge = format!(
        "Bearer realm=\"{}/token\",service=\"registry\"",
        server.uri()
    );
    Mock::given(path("/v2/_catalog"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("WWW-Authenticate", challenge.clone()),
        )
        .mount(&server)
        .await;
    Mock::given(path("/token"))
        .and(query_param("scope", "registry:catalog:*"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .and(header("authorization", "Bearer readable"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"name":"demo/nginx","tags":["latest"]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/tags/list"))
        .respond_with(ResponseTemplate::new(401).insert_header("WWW-Authenticate", challenge))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(path("/token"))
        .and(query_param("scope", "repository:demo/nginx:pull"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token":"readable"})))
        .mount(&server)
        .await;
    Mock::given(path("/v2/demo/nginx/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(index()))
        .mount(&server)
        .await;
    let result = client(&server)
        .browse(&registry(), &["nginx".into()], None)
        .await
        .unwrap();
    assert_eq!(result.scope, "known");
    assert!(!result.complete);
    assert_eq!(result.images.len(), 1);
}
#[rocket::async_test]
async fn mixed_unknown_or_malformed_descriptors_are_not_reported_as_complete() {
    for descriptor in [
        json!({"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":format!("sha256:{}","e".repeat(64)),"size":200}),
        json!({"mediaType":"application/vnd.oci.image.manifest.v1+json","size":200,"platform":{"os":"linux","architecture":"arm64"}}),
    ] {
        let server = MockServer::start().await;
        tags(&server, "nginx").await;
        let mut manifest = index();
        manifest["manifests"]
            .as_array_mut()
            .unwrap()
            .push(descriptor);
        Mock::given(path("/v2/demo/nginx/manifests/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(manifest))
            .mount(&server)
            .await;
        let result = client(&server)
            .browse(&registry(), &[], Some("nginx"))
            .await
            .unwrap();
        assert!(!result.complete);
        assert!(result.images[0].error.is_some());
    }
}

#[rocket::async_test]
async fn index_without_platform_is_resolved_through_leaf_config() {
    let server = MockServer::start().await;
    tags(&server, "nginx").await;
    let config_digest = format!("sha256:{}", "f".repeat(64));
    let leaf = json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"digest":config_digest,"size":100},"layers":[]});
    use sha2::{Digest, Sha256};
    let leaf_digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&leaf).unwrap()))
    );
    let mut root = index();
    root["manifests"][1]
        .as_object_mut()
        .unwrap()
        .remove("platform");
    root["manifests"][1]["digest"] = leaf_digest.clone().into();
    Mock::given(path("/v2/demo/nginx/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(root))
        .mount(&server)
        .await;
    Mock::given(path(format!("/v2/demo/nginx/manifests/{leaf_digest}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(leaf))
        .mount(&server)
        .await;
    Mock::given(path(format!("/v2/demo/nginx/blobs/{config_digest}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"os":"linux","architecture":"arm64"})),
        )
        .mount(&server)
        .await;
    let view = client(&server)
        .browse(&registry(), &[], Some("nginx"))
        .await
        .unwrap();
    assert!(view.complete);
    assert_eq!(view.images[0].platforms, vec!["linux/amd64", "linux/arm64"]);
}

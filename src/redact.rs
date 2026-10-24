use crate::domain::TaskPayload;
pub fn redact(mut value: String, secrets: &[String]) -> String {
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        value = value.replace(secret, "[REDACTED]");
    }
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(100_000)
        .collect()
}
pub fn payload_secrets(payload: &TaskPayload) -> Vec<String> {
    use base64::Engine;
    payload
        .items
        .iter()
        .flat_map(|i| std::iter::once(&i.target_credentials).chain(i.source_credentials.iter()))
        .flat_map(|c| {
            [
                c.password.clone(),
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{}", c.username, c.password)),
            ]
        })
        .collect()
}

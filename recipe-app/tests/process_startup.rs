use std::{
    net::{SocketAddr, TcpListener},
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};
use tempfile::TempDir;

const CONFIGURED_TOKEN: &str = "configured-token-secret-sentinel";
const SECOND_CONFIGURED_TOKEN: &str = "second-configured-token-secret-sentinel";
const AUTHORIZATION_SENTINEL: &str = "authorization-header-secret-sentinel";

#[tokio::test]
async fn binary_starts_restarts_and_never_logs_tokens_or_authorization_headers() {
    let directory = TempDir::new().expect("temporary directory");
    let address = available_address();
    let config_file = directory.path().join("missing-config.toml");
    let database_path = directory.path().join("instance.sqlite3");
    let content_dir = directory.path().join("content");

    let child = start_binary(
        &config_file,
        &database_path,
        &content_dir,
        address,
        &format!("agent:{CONFIGURED_TOKEN},second-agent:{SECOND_CONFIGURED_TOKEN}"),
    );
    request_health(address).await;
    let first_logs = stop_and_capture(child);
    assert_logged_health_request(&first_logs);

    let restarted = start_binary(
        &config_file,
        &database_path,
        &content_dir,
        address,
        &format!("agent:{CONFIGURED_TOKEN},second-agent:{SECOND_CONFIGURED_TOKEN}"),
    );
    request_health(address).await;
    let second_logs = stop_and_capture(restarted);
    assert_logged_health_request(&second_logs);

    let logs = format!("{first_logs}\n{second_logs}");
    assert!(
        !logs.contains(CONFIGURED_TOKEN),
        "configured token leaked: {logs}"
    );
    assert!(
        !logs.contains(SECOND_CONFIGURED_TOKEN),
        "second configured token leaked: {logs}"
    );
    assert!(
        !logs.contains(AUTHORIZATION_SENTINEL),
        "Authorization header leaked: {logs}"
    );
    assert!(database_path.is_file());
    assert!(content_dir.is_dir());
}

#[tokio::test]
async fn invalid_file_configuration_reports_a_sanitized_error() {
    let directory = TempDir::new().expect("temporary directory");
    let config_file = directory.path().join("invalid.toml");
    tokio::fs::write(
        &config_file,
        format!("api_tokens = [\"{CONFIGURED_TOKEN}\"\n"),
    )
    .await
    .expect("write invalid config");

    let output = Command::new(env!("CARGO_BIN_EXE_recipe-app"))
        .env_clear()
        .env("APP_CONFIG_FILE", &config_file)
        .env("APP__API_TOKENS", format!("agent:{CONFIGURED_TOKEN}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run app with invalid configuration");
    assert!(
        !output.status.success(),
        "invalid config unexpectedly started"
    );
    let logs = output_text(output);

    assert!(
        logs.contains("invalid application configuration"),
        "sanitized diagnostic missing: {logs}"
    );
    assert!(
        !logs.contains(CONFIGURED_TOKEN),
        "configured token leaked: {logs}"
    );
    assert!(
        !logs.contains(AUTHORIZATION_SENTINEL),
        "Authorization sentinel leaked: {logs}"
    );
}

fn available_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve ephemeral port");
    listener.local_addr().expect("reserved address")
}

fn start_binary(
    config_file: &Path,
    database_path: &Path,
    content_dir: &Path,
    address: SocketAddr,
    tokens: &str,
) -> Child {
    Command::new(env!("CARGO_BIN_EXE_recipe-app"))
        .env_clear()
        .env("APP_CONFIG_FILE", config_file)
        .env("APP__BIND_ADDR", address.to_string())
        .env("APP__DATABASE_PATH", database_path)
        .env("APP__CONTENT_DIR", content_dir)
        .env("APP__API_TOKENS", tokens)
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start recipe app binary")
}

async fn request_health(address: SocketAddr) {
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if let Ok(response) = client
            .get(format!("http://{address}/health"))
            .bearer_auth(AUTHORIZATION_SENTINEL)
            .send()
            .await
        {
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert_eq!(
                response
                    .json::<serde_json::Value>()
                    .await
                    .expect("health JSON")["status"],
                "ok"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("recipe app did not become healthy at {address}");
}

fn stop_and_capture(mut child: Child) -> String {
    child.kill().expect("stop recipe app");
    output_text(child.wait_with_output().expect("capture process logs"))
}

fn output_text(output: Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_logged_health_request(logs: &str) {
    assert!(
        logs.contains("recipe app listening"),
        "startup log missing: {logs}"
    );
    assert!(
        logs.contains("\"configured_token_count\":2"),
        "both comma-separated environment tokens were not loaded: {logs}"
    );
    assert!(
        logs.contains("request received"),
        "request log missing: {logs}"
    );
    assert!(logs.contains("/health"), "health path log missing: {logs}");
}

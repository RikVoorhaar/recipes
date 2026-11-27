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
async fn healthcheck_probes_server_without_creating_data() {
    let directory = TempDir::new().unwrap();
    let address = available_address();
    let database = directory.path().join("db.sqlite");
    let content = directory.path().join("content");
    let probe = || {
        Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .arg("healthcheck")
            .env_clear()
            .env("APP_CONFIG_FILE", directory.path().join("missing.toml"))
            .env("APP__BIND_ADDR", format!("0.0.0.0:{}", address.port()))
            .env("APP__DATABASE_PATH", &database)
            .env("APP__CONTENT_DIR", &content)
            .output()
            .unwrap()
    };
    assert!(!probe().status.success());
    assert!(!database.exists());
    assert!(!content.exists());
    let child = start_binary(
        &directory.path().join("missing.toml"),
        &database,
        &content,
        address,
        "owner:secret",
    );
    request_health(address).await;
    assert!(probe().status.success());
    stop_and_capture(child);
    assert!(!probe().status.success());
}

#[test]
fn invalid_public_base_url_fails_before_startup() {
    let directory = TempDir::new().unwrap();
    for value in [
        "not-a-url",
        "https://example.com/?query=1",
        "https://user:secret@example.com",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .env_clear()
            .env("APP_CONFIG_FILE", directory.path().join("missing.toml"))
            .env("APP__CONTENT_DIR", directory.path().join("content"))
            .env("APP__PUBLIC_BASE_URL", value)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{value}");
        assert!(!directory.path().join("content").exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
    }
}

#[tokio::test]
async fn configured_public_base_url_is_used_by_binary() {
    let directory = TempDir::new().unwrap();
    let address = available_address();
    let child = Command::new(env!("CARGO_BIN_EXE_recipe-app"))
        .env_clear()
        .env("APP_CONFIG_FILE", directory.path().join("missing.toml"))
        .env("APP__BIND_ADDR", address.to_string())
        .env("APP__DATABASE_PATH", directory.path().join("db.sqlite"))
        .env("APP__CONTENT_DIR", directory.path().join("content"))
        .env("APP__API_TOKENS", "owner:secret")
        .env("APP__PUBLIC_BASE_URL", "https://recipes.example/prefix")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    request_health(address).await;
    let client = reqwest::Client::new();
    let draft = mcp_tool(
        &client,
        address,
        "save_draft",
        serde_json::json!({"document":{"title":"Soup","servings":2,"body":"Cook @carrot{1}."}}),
    )
    .await;
    let published = mcp_tool(
        &client,
        address,
        "publish_draft",
        serde_json::json!({"id":draft["id"],"expected_revision":1}),
    )
    .await;
    let id = published["recipe"]["id"].as_str().unwrap();
    assert_eq!(
        published["recipe"]["public_url"],
        format!("https://recipes.example/prefix/recipes/{id}")
    );
    assert_eq!(
        published["recipe"]["pdf_url"],
        format!("https://recipes.example/prefix/recipes/{id}.pdf")
    );
    stop_and_capture(child);
}

async fn mcp_tool(
    client: &reqwest::Client,
    address: SocketAddr,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let response: serde_json::Value = client.post(format!("http://{address}/mcp"))
        .bearer_auth("secret")
        .header("host", "untrusted.example")
        .header("accept", "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .send().await.unwrap().json().await.unwrap();
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn absent_origin_configuration_denies_origins_but_not_non_browser_requests() {
    let directory = TempDir::new().unwrap();
    let address = available_address();
    let child = start_binary(
        &directory.path().join("missing.toml"),
        &directory.path().join("db.sqlite"),
        &directory.path().join("content"),
        address,
        "owner:secret",
    );
    request_health(address).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"});
    let url = format!("http://{address}/mcp");
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("secret")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("secret")
            .header("origin", "https://recipes.rikvoorhaar.com")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    stop_and_capture(child);
}

#[tokio::test]
async fn configured_browser_origins_are_enforced_by_the_binary() {
    let directory = TempDir::new().unwrap();
    let address = available_address();
    let child = Command::new(env!("CARGO_BIN_EXE_recipe-app"))
        .env_clear()
        .env("APP_CONFIG_FILE", directory.path().join("missing.toml"))
        .env("APP__BIND_ADDR", address.to_string())
        .env("APP__DATABASE_PATH", directory.path().join("db.sqlite"))
        .env("APP__CONTENT_DIR", directory.path().join("content"))
        .env("APP__API_TOKENS", "owner:secret")
        .env(
            "APP__MCP_ALLOWED_ORIGINS",
            "http://localhost:6274,https://trusted.example",
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    request_health(address).await;
    let client = reqwest::Client::new();
    for (origin, status) in [
        ("https://trusted.example", 200),
        ("https://bad.example", 403),
    ] {
        let response = client
            .post(format!("http://{address}/mcp"))
            .bearer_auth("secret")
            .header("origin", origin)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    stop_and_capture(child);
}

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

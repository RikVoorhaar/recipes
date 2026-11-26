use serde_json::{json, Value};

/// Call an authenticated MCP tool and decode its JSON text result.
pub async fn tool(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let result = call(client, base, token, name, arguments).await;
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(result["content"][0]["text"].as_str().expect("tool text"))
        .expect("tool JSON")
}

/// Return the tool result, including errors and non-text content.
pub async fn call(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .send()
        .await
        .expect("MCP request");
    assert_eq!(response.status(), 200);
    let response: Value = response.json().await.expect("MCP JSON response");
    assert!(response["result"].is_object(), "{response}");
    response["result"].clone()
}

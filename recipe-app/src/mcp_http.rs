//! Stateless Streamable HTTP transport. Authentication is performed on every POST.
use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::{authorize_secret, mcp, RecipeService};

pub(crate) fn valid_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https") && url.origin().ascii_serialization() == origin
}

fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values
        .next()
        .map(|v| v.to_str().map_err(|_| ()))
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn credential(headers: &HeaderMap) -> Result<&str, ()> {
    let auth = one_header(headers, "authorization")?;
    let key = one_header(headers, "x-api-key")?;
    let secret = match (auth, key) {
        (Some(auth), None) => {
            let (scheme, value) = auth.split_once(' ').ok_or(())?;
            if !scheme.eq_ignore_ascii_case("bearer") {
                return Err(());
            }
            value
        }
        (None, Some(key)) => key,
        _ => return Err(()),
    };
    if secret.is_empty() || secret.bytes().any(|b| b.is_ascii_whitespace() || b == b',') {
        return Err(());
    }
    Ok(secret)
}

fn rpc_error(code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":null,"error":{"code":code,"message":message}})
}

fn modern_error(message: &Value, code: i32, text: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "jsonrpc":"2.0", "id":message.get("id").cloned().unwrap_or(Value::Null),
            "error":{"code":code,"message":text}
        })),
    )
        .into_response()
}

fn decoded_name(value: &str) -> Option<String> {
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        String::from_utf8(STANDARD.decode(encoded).ok()?).ok()
    } else if value.trim() == value
        && value
            .bytes()
            .all(|b| b == b' ' || b == b'\t' || (0x21..=0x7e).contains(&b))
    {
        Some(value.to_owned())
    } else {
        None
    }
}

fn modern_validation(headers: &HeaderMap, message: &Value) -> Result<(), Box<Response>> {
    let mismatch = || {
        Box::new(modern_error(
            message,
            -32020,
            "Missing, malformed, or inconsistent MCP header",
        ))
    };
    let version = one_header(headers, "mcp-protocol-version")
        .map_err(|_| mismatch())?
        .ok_or_else(mismatch)?;
    let method = one_header(headers, "mcp-method")
        .map_err(|_| mismatch())?
        .ok_or_else(mismatch)?;
    if message["method"].as_str() != Some(method) {
        return Err(mismatch());
    }
    let meta = &message["params"]["_meta"];
    let body_version = meta
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str);
    if body_version.is_some_and(|v| v != version) {
        return Err(mismatch());
    }
    if matches!(method, "tools/call" | "resources/read" | "prompts/get") {
        let name = one_header(headers, "mcp-name")
            .map_err(|_| mismatch())?
            .and_then(decoded_name)
            .ok_or_else(mismatch)?;
        let field = if method == "resources/read" {
            "uri"
        } else {
            "name"
        };
        if message["params"][field].as_str() != Some(&name) {
            return Err(mismatch());
        }
    }
    if body_version.is_none()
        || !meta
            .get("io.modelcontextprotocol/clientCapabilities")
            .is_some_and(Value::is_object)
    {
        return Err(Box::new(modern_error(
            message,
            -32602,
            "Missing or invalid protocol metadata",
        )));
    }
    if version != mcp::MODERN_PROTOCOL {
        return Err(Box::new((StatusCode::BAD_REQUEST, Json(json!({
            "jsonrpc":"2.0","id":message.get("id").cloned().unwrap_or(Value::Null),
            "error":{"code":-32022,"message":"Unsupported protocol version","data":{"supported":mcp::ALL_PROTOCOLS,"requested":version}}
        }))).into_response()));
    }
    Ok(())
}

fn valid_request(message: &Value) -> bool {
    message.is_object()
        && message["jsonrpc"] == "2.0"
        && message["method"].as_str().is_some()
        && message
            .get("id")
            .is_none_or(|id| id.is_string() || id.is_number() || id.is_null())
        && message
            .get("params")
            .is_none_or(|p| p.is_object() || p.is_array())
}

fn modern_message(message: &Value) -> bool {
    match message {
        Value::Array(messages) => messages.iter().any(modern_message),
        _ => {
            message["method"] == "server/discover"
                || message
                    .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
                    .is_some()
        }
    }
}

pub(crate) async fn check_origin(
    State(origins): State<Arc<Vec<String>>>,
    request: Request,
    next: Next,
) -> Response {
    let origin = match one_header(request.headers(), "origin") {
        Ok(None) => None,
        Ok(Some(origin)) if valid_origin(origin) && origins.iter().any(|o| o == origin) => {
            Some(origin.to_owned())
        }
        _ => return StatusCode::FORBIDDEN.into_response(),
    };
    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            origin.parse().expect("validated origin header"),
        );
        headers.insert(header::VARY, "Origin".parse().unwrap());
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "POST, GET, OPTIONS".parse().unwrap(),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "authorization, content-type, x-api-key, mcp-protocol-version, mcp-method, mcp-name"
                .parse()
                .unwrap(),
        );
    }
    response
}

pub(crate) async fn post(
    State(state): State<RecipeService>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let caller = match credential(&headers) {
        Ok(secret) => authorize_secret(&state, secret).await,
        Err(()) => Err(crate::ApiError::new("unauthorized", "A token is required")),
    };
    let caller = match caller {
        Ok(caller) => caller,
        Err(error) if error.code == "unauthorized" => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
            )
                .into_response()
        }
        Err(error) => return error.into_response(),
    };
    let version_header = one_header(&headers, "mcp-protocol-version");
    if !matches!(one_header(&headers, "content-type"), Ok(Some(value)) if value.split(';').next().is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json")))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(-32700, "Parse error")),
            )
                .into_response()
        }
    };
    let modern = headers
        .get_all("mcp-protocol-version")
        .iter()
        .any(|value| value.as_bytes() == mcp::MODERN_PROTOCOL.as_bytes())
        || modern_message(&message)
        || headers.contains_key("mcp-method");
    let version = match version_header {
        // No session state is kept, so the backwards-compatibility default applies.
        Ok(None) => "2025-03-26",
        Ok(Some(version)) => version,
        Err(()) if modern => {
            return modern_error(
                &message,
                -32020,
                "Missing, malformed, or inconsistent MCP header",
            );
        }
        Err(()) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !modern && !mcp::SUPPORTED_PROTOCOLS.contains(&version) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if let Value::Array(messages) = message {
        if modern || !matches!(version, "2025-03-26" | "2024-11-05") || messages.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(rpc_error(-32600, "Invalid Request")),
            )
                .into_response();
        }
        let mut responses = Vec::new();
        for message in messages {
            if !valid_request(&message) {
                responses.push(rpc_error(-32600, "Invalid Request"));
            } else if let Some(response) = mcp::handle(&state, &caller, message).await {
                responses.push(response);
            }
        }
        return if responses.is_empty() {
            StatusCode::ACCEPTED.into_response()
        } else {
            Json(responses).into_response()
        };
    }
    if !valid_request(&message) {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(-32600, "Invalid Request")),
        )
            .into_response();
    }
    if modern {
        if message
            .get("id")
            .is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64()))
        {
            return modern_error(&message, -32600, "Invalid Request");
        }
        if let Err(response) = modern_validation(&headers, &message) {
            return *response;
        }
        if message.get("id").is_some()
            && message["method"] == "tools/call"
            && message["params"]
                .get("arguments")
                .is_some_and(|arguments| !arguments.is_object())
        {
            return Json(json!({
                "jsonrpc":"2.0", "id":message["id"],
                "error":{"code":-32602,"message":"Invalid params: arguments must be an object"}
            }))
            .into_response();
        }
        if message.get("id").is_some()
            && message["method"] == "tools/call"
            && !mcp::known_tool(message["params"]["name"].as_str().unwrap_or(""))
        {
            return Json(json!({
                "jsonrpc":"2.0", "id":message["id"],
                "error":{"code":-32602,"message":"Unknown tool"}
            }))
            .into_response();
        }
    }
    let cacheable = modern
        && matches!(
            message["method"].as_str(),
            Some("server/discover" | "tools/list" | "resources/list" | "resources/read")
        );
    match mcp::handle(&state, &caller, message).await {
        Some(mut response) => {
            if modern {
                if let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) {
                    result.insert("resultType".into(), json!("complete"));
                    result.insert("_meta".into(), json!({"io.modelcontextprotocol/serverInfo":{"name":"recipe-mcp","version":"0.1.0"}}));
                    if cacheable {
                        // Restrict reuse to the same authorization context; freshness
                        // expires immediately even for otherwise shared content.
                        result.insert("ttlMs".into(), json!(0));
                        result.insert("cacheScope".into(), json!("private"));
                    }
                }
                if response["error"]["code"] == -32601 {
                    return (StatusCode::NOT_FOUND, Json(response)).into_response();
                }
            }
            Json(response).into_response()
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::valid_origin;

    #[test]
    fn origins_must_be_canonical_http_origins() {
        for origin in ["https://example.com", "http://localhost:6274"] {
            assert!(valid_origin(origin), "{origin}");
        }
        for origin in [
            "null",
            "*",
            "https://example.com/",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com?query",
            "http://example.com:80",
            "file://example.com",
        ] {
            assert!(!valid_origin(origin), "{origin}");
        }
    }
}

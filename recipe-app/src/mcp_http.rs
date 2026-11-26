//! Stateless Streamable HTTP transport. Authentication is performed on every POST.
use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
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
            "authorization, content-type, x-api-key, mcp-protocol-version"
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
    match one_header(&headers, "mcp-protocol-version") {
        Ok(None) => {}
        Ok(Some(version)) if mcp::SUPPORTED_PROTOCOLS.contains(&version) => {}
        _ => return StatusCode::BAD_REQUEST.into_response(),
    }
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
    let valid = message.is_object()
        && message["jsonrpc"] == "2.0"
        && message["method"].as_str().is_some()
        && message
            .get("id")
            .is_none_or(|id| id.is_string() || id.is_number() || id.is_null())
        && message
            .get("params")
            .is_none_or(|p| p.is_object() || p.is_array());
    if !valid {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(-32600, "Invalid Request")),
        )
            .into_response();
    }
    match mcp::handle(&state, &caller, message).await {
        Some(response) => Json(response).into_response(),
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

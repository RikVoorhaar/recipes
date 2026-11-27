#[allow(dead_code)]
mod support;

#[tokio::test]
async fn health_and_restart_with_an_empty_database() {
    let fixture = support::TestApp::start().await;
    let response = fixture.get("/health").await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("JSON body")["status"],
        "ok"
    );
    let restarted = fixture.restart().await;
    assert_eq!(restarted.migration_count().await, 8);
    let response = restarted.get("/health").await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("JSON body")["status"],
        "ok"
    );
}

#[tokio::test]
async fn removed_openapi_route_returns_not_found() {
    let fixture = support::TestApp::start().await;
    assert_eq!(
        fixture.get("/api/v1/openapi.yaml").await.status(),
        axum::http::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn removed_rest_routes_return_not_found() {
    let fixture = support::TestApp::start().await;
    let client = fixture.client();
    for (method, path) in [
        (reqwest::Method::GET, "/api/v1/recipes"),
        (reqwest::Method::POST, "/api/v1/recipes"),
        (reqwest::Method::GET, "/api/v1/recipes/missing/pdf"),
        (reqwest::Method::GET, "/api/v1/drafts"),
        (reqwest::Method::POST, "/api/v1/images"),
        (reqwest::Method::GET, "/api/v1/search"),
        (reqwest::Method::PUT, "/api/v1/ingredients/missing"),
        (reqwest::Method::GET, "/api/v1/anything"),
    ] {
        let response = client
            .request(method, fixture.url(path))
            .bearer_auth("test-secret")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}

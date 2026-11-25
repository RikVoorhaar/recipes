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
    assert_eq!(restarted.migration_count().await, 6);
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

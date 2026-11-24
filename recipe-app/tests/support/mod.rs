use recipe_app::{app_with_storage, database, storage::FileImageStorage};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::{net::SocketAddr, path::PathBuf};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

pub struct TestApp {
    _directory: TempDir,
    database_path: PathBuf,
    address: SocketAddr,
    server: JoinHandle<()>,
    pool: SqlitePool,
}

impl TestApp {
    pub async fn start() -> Self {
        Self::launch(TempDir::new().expect("temporary test directory"), None).await
    }

    async fn launch(directory: TempDir, address: Option<SocketAddr>) -> Self {
        let database_path = directory.path().join("test.sqlite3");
        let content_dir = directory.path().join("content");
        tokio::fs::create_dir_all(&content_dir)
            .await
            .expect("content directory");
        let pool = database::connect(&database_path).await.expect("database");
        database::migrate(&pool).await.expect("migrations");
        recipe_app::ensure_derived(&pool)
            .await
            .expect("derived data initialization");
        recipe_app::configure_tokens(&pool, &["integration:test-secret".into()])
            .await
            .expect("test token configuration");
        let listener = TcpListener::bind(address.unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0))))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("local address");
        let app_pool = pool.clone();
        let storage = Arc::new(FileImageStorage::new(content_dir));
        let server = tokio::spawn(async move {
            axum::serve(listener, app_with_storage(app_pool, storage))
                .await
                .expect("server");
        });
        Self {
            _directory: directory,
            database_path,
            address,
            server,
            pool,
        }
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::get(format!("http://{}{path}", self.address))
            .await
            .expect("HTTP request")
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    pub async fn migration_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .expect("migration history")
    }

    pub async fn revision_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM recipe_revisions")
            .fetch_one(&self.pool)
            .await
            .expect("recipe revision count")
    }

    pub async fn ingredient_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM ingredients")
            .fetch_one(&self.pool)
            .await
            .expect("ingredient count")
    }

    pub async fn stored_token(&self) -> (String, String) {
        sqlx::query_as("SELECT token_hash, label FROM api_tokens LIMIT 1")
            .fetch_one(&self.pool)
            .await
            .expect("stored token digest")
    }

    pub async fn rebuild_derived(&self) {
        recipe_app::rebuild_derived(&self.pool)
            .await
            .expect("rebuild derived recipe data");
    }

    pub async fn set_recipe_updated_at(&self, recipe_id: &str, timestamp: &str) {
        sqlx::query(
            "UPDATE recipe_revisions SET created_at=? WHERE id=(SELECT current_revision_id FROM recipes WHERE id=?)",
        )
        .bind(timestamp)
        .bind(recipe_id)
        .execute(&self.pool)
        .await
        .expect("set fixture update timestamp");
    }

    pub async fn ingredient_history_count(&self, ingredient_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM ingredient_history WHERE ingredient_id=?")
            .bind(ingredient_id)
            .fetch_one(&self.pool)
            .await
            .expect("ingredient history count")
    }

    pub async fn add_ingredient_alias_fixture(
        &self,
        alias: &str,
        normalized_alias: &str,
        ingredient_id: &str,
    ) {
        sqlx::query(
            "INSERT INTO ingredient_aliases(normalized_alias,alias,ingredient_id) VALUES(?,?,?)",
        )
        .bind(normalized_alias)
        .bind(alias)
        .bind(ingredient_id)
        .execute(&self.pool)
        .await
        .expect("insert legacy alias fixture");
    }

    pub async fn restart(self) -> Self {
        let Self {
            _directory,
            database_path,
            address,
            server,
            pool: _,
        } = self;
        server.abort();
        let pool = database::connect(&database_path)
            .await
            .expect("reopen database");
        database::migrate(&pool).await.expect("rerun migrations");
        let listener = TcpListener::bind(address).await.expect("rebind listener");
        let app_pool = pool.clone();
        let storage = Arc::new(FileImageStorage::new(_directory.path().join("content")));
        let server = tokio::spawn(async move {
            axum::serve(listener, app_with_storage(app_pool, storage))
                .await
                .expect("restarted server");
        });
        Self {
            _directory,
            database_path,
            address,
            server,
            pool,
        }
    }
}

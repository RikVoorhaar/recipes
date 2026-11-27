use recipe_app::database;
use sha2::{Digest, Sha384};
use sqlx::{sqlite::SqliteConnectOptions, ConnectOptions, Connection, SqliteConnection};
use tempfile::TempDir;

async fn apply_stage_two_schema(connection: &mut SqliteConnection) {
    let migrations = [
        (
            1_i64,
            "bootstrap",
            include_str!("../migrations/0001_bootstrap.sql"),
        ),
        (
            2_i64,
            "recipes",
            include_str!("../migrations/0002_recipes.sql"),
        ),
    ];
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)",
    )
    .execute(&mut *connection)
    .await
    .unwrap();
    for (version, description, sql) in migrations {
        sqlx::raw_sql(sql).execute(&mut *connection).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(?,?,1,?,0)")
            .bind(version)
            .bind(description)
            .bind(Sha384::digest(sql.as_bytes()).to_vec())
            .execute(&mut *connection)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn migration_preserves_tokens_drafts_candidates_and_orphaned_drafts() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    let mut connection = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    apply_stage_two_schema(&mut connection).await;
    for (version, description, sql) in [
        (
            3_i64,
            "derived_ingredients",
            include_str!("../migrations/0003_derived_ingredients.sql"),
        ),
        (
            4,
            "ingredient_resolution_provenance",
            include_str!("../migrations/0004_ingredient_resolution_provenance.sql"),
        ),
        (5, "images", include_str!("../migrations/0005_images.sql")),
        (6, "drafts", include_str!("../migrations/0006_drafts.sql")),
        (
            7,
            "draft_cover_candidates",
            include_str!("../migrations/0007_draft_cover_candidates.sql"),
        ),
    ] {
        sqlx::raw_sql(sql).execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(?,?,1,?,0)")
            .bind(version).bind(description).bind(Sha384::digest(sql.as_bytes()).to_vec())
            .execute(&mut connection).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO api_tokens VALUES('02811e4a991a586c7538db3f3e087c2d25b3a503cdb755f29e550114af13731b','owner');
        INSERT INTO api_tokens VALUES('c2f225c53e902e19125a5881958950fb9469c5b15006d20049c4f2bde2b112e3','owner');
        INSERT INTO recipe_drafts VALUES('d1','02811e4a991a586c7538db3f3e087c2d25b3a503cdb755f29e550114af13731b',1,NULL,NULL,'{}','yesterday','today');
        INSERT INTO recipe_drafts VALUES('d3','c2f225c53e902e19125a5881958950fb9469c5b15006d20049c4f2bde2b112e3',1,NULL,NULL,'{}','yesterday','today');
        INSERT INTO recipe_drafts VALUES('d2','orphan-hash',1,NULL,NULL,'{}','yesterday','today');
        INSERT INTO draft_cover_candidates VALUES('image','d1','02811e4a991a586c7538db3f3e087c2d25b3a503cdb755f29e550114af13731b',1,'today');",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let pool = database::connect(&path).await.unwrap();
    database::migrate(&pool).await.unwrap();
    let row: (String, String, String, i64) = sqlx::query_as(
        "SELECT t.token_hash,d.owner_token_id,c.owner_token_id,t.revoked FROM api_tokens t JOIN recipe_drafts d ON d.owner_token_id=t.id JOIN draft_cover_candidates c ON c.draft_id=d.id WHERE t.label='owner'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(
        row.0,
        "02811e4a991a586c7538db3f3e087c2d25b3a503cdb755f29e550114af13731b"
    );
    assert_eq!(row.1, row.2);
    assert_eq!(row.3, 0);
    let revoked: i64 = sqlx::query_scalar("SELECT t.revoked FROM api_tokens t JOIN recipe_drafts d ON d.owner_token_id=t.id WHERE d.id='d2'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(revoked, 1);
    let second_id: String =
        sqlx::query_scalar("SELECT owner_token_id FROM recipe_drafts WHERE id='d3'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(row.1, second_id);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = recipe_app::app(pool.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let ping = |secret: &str| {
        client
            .post(&url)
            .bearer_auth(secret)
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
    };
    assert_eq!(ping("legacy-first").send().await.unwrap().status(), 200);
    assert_eq!(ping("legacy-second").send().await.unwrap().status(), 200);
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .env("APP__DATABASE_PATH", &path)
            .env("APP__CONTENT_DIR", directory.path().join("content"))
            .args(args)
            .output()
            .unwrap()
    };
    let listing = run(&["token", "list"]);
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains(&row.1) && listing.contains(&second_id));
    assert!(!run(&["token", "rotate", "--label", "owner"])
        .status
        .success());
    let rotated = run(&["token", "rotate", "--id", &row.1]);
    assert!(rotated.status.success());
    let replacement = String::from_utf8(rotated.stdout).unwrap();
    assert!(replacement.starts_with("rcp_"));
    assert!(run(&["token", "revoke", "--id", &second_id])
        .status
        .success());
    assert_eq!(ping("legacy-first").send().await.unwrap().status(), 401);
    assert_eq!(ping("legacy-second").send().await.unwrap().status(), 401);
    assert_eq!(ping(replacement.trim()).send().await.unwrap().status(), 200);
    let owners: Vec<(String, String, i64)> = sqlx::query_as("SELECT d.id,t.id,t.revoked FROM recipe_drafts d JOIN api_tokens t ON t.id=d.owner_token_id WHERE d.id IN ('d1','d3') ORDER BY d.id")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(
        owners,
        vec![("d1".into(), row.1, 0), ("d3".into(), second_id, 1)]
    );
}

#[tokio::test]
async fn populated_stage_two_database_migrates_and_backfills_ingredient_data() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("stage-two.sqlite3");
    let mut connection = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    apply_stage_two_schema(&mut connection).await;
    sqlx::query(
        "INSERT INTO recipes(id,current_revision_id,deleted) VALUES('recipe-old','revision-old',0)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body,deleted) VALUES('revision-old','recipe-old','2025-01-01T12:00:00Z','old-agent','Old recipe','kept',2,'[\"family\"]','@Straße{100%g}',0)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let pool = database::connect(&path).await.unwrap();
    database::migrate(&pool).await.unwrap();
    recipe_app::rebuild_derived(&pool).await.unwrap();

    let current: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM recipe_revisions WHERE id='revision-old' AND title='Old recipe'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let linked: (String, String) = sqlx::query_as(
        "SELECT o.wording,i.name FROM ingredient_occurrences o JOIN ingredients i ON i.id=o.ingredient_id WHERE o.revision_id='revision-old'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let provenance: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ingredient_resolution_provenance WHERE revision_id='revision-old'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(current, 1);
    assert_eq!(linked, ("Straße".into(), "Straße".into()));
    assert_eq!(provenance, 1);
}

#[tokio::test]
async fn populated_stage_three_database_migration_keeps_saved_alias_resolution() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("stage-three.sqlite3");
    let mut connection = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    apply_stage_two_schema(&mut connection).await;
    let stage_three = include_str!("../migrations/0003_derived_ingredients.sql");
    sqlx::raw_sql(stage_three)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(3,'derived_ingredients',1,?,0)")
        .bind(Sha384::digest(stage_three.as_bytes()).to_vec())
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO recipes(id,current_revision_id,deleted) VALUES('recipe-v3','revision-v3',0)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO recipe_revisions(id,recipe_id,created_at,author_label,title,description,servings,tags,body,deleted) VALUES('revision-v3','recipe-v3','2025-01-01T12:00:00Z','old-agent','Old alias recipe','',2,'[]','@allium sativum{1}',0)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ingredients(id,name,normalized_name) VALUES('ingredient-garlic','garlic','garlic')")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ingredient_occurrences(revision_id,ordinal,wording,normalized_wording,ingredient_id) VALUES('revision-v3',0,'allium sativum','allium sativum','ingredient-garlic')")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let pool = database::connect(&path).await.unwrap();
    database::migrate(&pool).await.unwrap();
    recipe_app::rebuild_derived(&pool).await.unwrap();
    let linked: String = sqlx::query_scalar(
        "SELECT ingredient_id FROM ingredient_occurrences WHERE revision_id='revision-v3'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let provenance: String = sqlx::query_scalar(
        "SELECT ingredient_id FROM ingredient_resolution_provenance WHERE revision_id='revision-v3'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(linked, "ingredient-garlic");
    assert_eq!(provenance, "ingredient-garlic");
}

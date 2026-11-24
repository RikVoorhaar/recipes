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

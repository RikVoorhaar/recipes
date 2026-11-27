//! Host-local credential management. Secrets are returned only on creation or rotation.
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

#[derive(FromRow)]
pub struct TokenInfo {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
}

fn secret() -> String {
    format!("rcp_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 128
        && !label.chars().any(char::is_control)
        && label.trim() == label
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<TokenInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,label,created_at,last_used_at,revoked FROM api_tokens ORDER BY label,id",
    )
    .fetch_all(pool)
    .await
}

pub async fn rotate_id(pool: &SqlitePool, id: &str) -> Result<String, sqlx::Error> {
    let value = secret();
    let result = sqlx::query("UPDATE api_tokens SET token_hash=? WHERE id=? AND revoked=0")
        .bind(super::token_hash(&value))
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol("active token ID not found".into()));
    }
    Ok(value)
}

pub async fn revoke_id(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE api_tokens SET revoked=1 WHERE id=? AND revoked=0")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol("active token ID not found".into()));
    }
    Ok(())
}

pub async fn create(pool: &SqlitePool, label: &str) -> Result<String, sqlx::Error> {
    if !valid_label(label) {
        return Err(sqlx::Error::Protocol("invalid label".into()));
    }
    let value = secret();
    let mut tx = pool.begin().await?;
    // Acquire the SQLite writer lock before testing for duplicates.
    sqlx::query("UPDATE api_tokens SET label=label WHERE label=?")
        .bind(label)
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_tokens WHERE label=?)")
        .bind(label)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(sqlx::Error::Protocol("label already exists".into()));
    }
    sqlx::query("INSERT INTO api_tokens(token_hash,label,id,created_at) VALUES(?,?,?,?)")
        .bind(super::token_hash(&value))
        .bind(label)
        .bind(Uuid::new_v4().to_string())
        .bind(super::now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(value)
}

pub async fn rotate(pool: &SqlitePool, label: &str) -> Result<String, sqlx::Error> {
    let value = secret();
    let result = sqlx::query("UPDATE api_tokens SET token_hash=? WHERE label=? AND revoked=0 AND (SELECT count(*) FROM api_tokens WHERE label=?)=1")
        .bind(super::token_hash(&value)).bind(label).bind(label).execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "active token label not found or ambiguous".into(),
        ));
    }
    Ok(value)
}

pub async fn revoke(pool: &SqlitePool, label: &str) -> Result<(), sqlx::Error> {
    let result = sqlx::query("UPDATE api_tokens SET revoked=1 WHERE label=? AND revoked=0 AND (SELECT count(*) FROM api_tokens WHERE label=?)=1")
        .bind(label).bind(label).execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "active token label not found or ambiguous".into(),
        ));
    }
    Ok(())
}

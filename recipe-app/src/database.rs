use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
    SqlitePool,
};
use std::{error::Error, path::Path, time::Duration};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn connect(path: &Path) -> Result<SqlitePool, Box<dyn Error + Send + Sync>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);
    Ok(SqlitePool::connect_with(options).await?)
}

pub async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

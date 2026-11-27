use recipe_app::{
    app_with_public_base_url, book_archive, book_snapshot, config::Settings, cover::Generator,
    database, storage::FileImageStorage,
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let settings = Settings::load()?;
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("import") {
        if args.len() != 4 || args[2] != "--src" {
            return Err("usage: recipe-app import --src PATH".into());
        }
        book_archive::import_tree(
            std::path::Path::new(&args[3]),
            &settings.database_path,
            &settings.content_dir,
        )
        .await?;
        println!("Book imported.");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("export") {
        if args.len() != 4 || args[2] != "--dest" {
            return Err("usage: recipe-app export --dest PATH".into());
        }
        let dest = std::path::Path::new(&args[3]);
        book_archive::safe_path(dest)?;
        book_archive::safe_path(&settings.database_path)?;
        book_archive::safe_path(&settings.content_dir)?;
        let absolute = if dest.exists() {
            dest.canonicalize()?
        } else {
            let parent = dest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            parent
                .canonicalize()?
                .join(dest.file_name().ok_or("invalid destination")?)
        };
        let database = settings.database_path.canonicalize()?;
        let content = settings.content_dir.canonicalize()?;
        if database.starts_with(&absolute)
            || content.starts_with(&absolute)
            || absolute.starts_with(&content)
        {
            return Err("export destination overlaps application data".into());
        }
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database)
            .read_only(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = sqlx::SqlitePool::connect_with(options).await?;
        let snapshot = book_snapshot::capture(&pool, &book_archive::ExportImages(content)).await?;
        book_archive::write_tree(dest, snapshot)?;
        println!("Book exported.");
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let mut address: SocketAddr = settings.bind_addr.parse()?;
        if address.ip().is_unspecified() {
            address.set_ip(match address.ip() {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            });
        }
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?
            .get(format!("http://{address}/health"))
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }
    tokio::fs::create_dir_all(&settings.content_dir).await?;
    let pool = database::connect(&settings.database_path).await?;
    database::migrate(&pool).await?;
    if std::env::args().nth(1).as_deref() == Some("rebuild-derived") {
        recipe_app::rebuild_derived(&pool).await?;
        println!("Derived ingredient links rebuilt.");
        return Ok(());
    }
    recipe_app::ensure_derived(&pool).await?;
    recipe_app::configure_tokens(&pool, &settings.api_tokens).await?;
    let cover_generator = match settings.openrouter_api_key {
        Some(key) if !key.trim().is_empty() => {
            let prompt = settings
                .cover_prompt_file
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()?;
            Some(Arc::new(Generator::new(
                key,
                Some(settings.cover_model),
                prompt,
            )?))
        }
        _ => None,
    };

    let address: SocketAddr = settings.bind_addr.parse()?;
    let listener = TcpListener::bind(address).await?;
    tracing::info!(
        address = %listener.local_addr()?,
        configured_token_count = settings.api_tokens.len(),
        "recipe app listening"
    );
    axum::serve(
        listener,
        app_with_public_base_url(
            pool,
            Arc::new(FileImageStorage::new(settings.content_dir)),
            cover_generator,
            settings.mcp_allowed_origins,
            settings
                .public_base_url
                .as_deref()
                .and_then(recipe_app::config::parse_public_base_url),
        ),
    )
    .await?;
    Ok(())
}

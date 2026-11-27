use recipe_app::{
    app_with_public_base_url, config::Settings, cover::Generator, database,
    storage::FileImageStorage,
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("healthcheck") {
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
    if args.first().map(String::as_str) == Some("token") {
        match args.as_slice() {
            [_, command] if command == "list" => {
                println!("LABEL\tCREATED\tLAST USED\tSTATUS");
                for token in recipe_app::token::list(&pool).await? {
                    println!(
                        "{}\t{}\t{}\t{}",
                        token.label,
                        token.created_at,
                        token.last_used_at.as_deref().unwrap_or("-"),
                        if token.revoked { "revoked" } else { "active" }
                    );
                }
            }
            [_, command, flag, label] if flag == "--label" => match command.as_str() {
                "create" => println!("{}", recipe_app::token::create(&pool, label).await?),
                "rotate" => println!("{}", recipe_app::token::rotate(&pool, label).await?),
                "revoke" => recipe_app::token::revoke(&pool, label).await?,
                _ => return Err("invalid token command".into()),
            },
            _ => return Err("usage: token list | token create|rotate|revoke --label LABEL".into()),
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("rebuild-derived") {
        recipe_app::rebuild_derived(&pool).await?;
        println!("Derived ingredient links rebuilt.");
        return Ok(());
    }
    recipe_app::ensure_derived(&pool).await?;
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

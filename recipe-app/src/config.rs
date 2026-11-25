use config::{Config, Environment, File};
use serde::Deserialize;
use std::{error::Error, io, path::PathBuf};

#[derive(Deserialize)]
pub struct Settings {
    pub bind_addr: String,
    pub database_path: PathBuf,
    pub content_dir: PathBuf,
    #[serde(default)]
    pub api_tokens: Vec<String>,
    #[serde(default = "default_cover_model")]
    pub cover_model: String,
    #[serde(default)]
    pub cover_prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
}

fn default_cover_model() -> String {
    "black-forest-labs/flux-3-image-klein".to_owned()
}

impl Settings {
    pub fn load() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let config_file = match std::env::var("APP_CONFIG_FILE") {
            Ok(path) => path,
            Err(std::env::VarError::NotPresent) => "config.toml".into(),
            Err(std::env::VarError::NotUnicode(_)) => return Err(invalid_configuration().into()),
        };
        let environment_tokens = match std::env::var("APP__API_TOKENS") {
            Ok(value) => Some(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            ),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => return Err(invalid_configuration().into()),
        };
        let settings = Config::builder()
            .set_default("bind_addr", "127.0.0.1:3000")
            .map_err(|_| invalid_configuration())?
            .set_default("database_path", "data/recipes.sqlite3")
            .map_err(|_| invalid_configuration())?
            .set_default("content_dir", "data/content")
            .map_err(|_| invalid_configuration())?
            .set_default("cover_model", default_cover_model())
            .map_err(|_| invalid_configuration())?
            .add_source(File::from(PathBuf::from(config_file)).required(false))
            .add_source(Environment::with_prefix("APP").separator("__"));
        let settings = if let Some(tokens) = environment_tokens {
            settings
                .set_override("api_tokens", tokens)
                .map_err(|_| invalid_configuration())?
        } else {
            settings
        };
        let settings = settings.build().map_err(|_| invalid_configuration())?;
        settings
            .try_deserialize()
            .map_err(|_| invalid_configuration().into())
    }
}

fn invalid_configuration() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid application configuration",
    )
}

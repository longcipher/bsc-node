//! Example CLI binary crate for this workspace template.

#![expect(clippy::print_stdout)]

use std::path::PathBuf;

use clap::Parser;
use common::prelude::*;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// CLI example for this workspace template.
#[derive(Debug, Parser)]
#[command(name = "cli-app", version, about = "CLI example for this workspace template")]
struct Args {
    /// Name to greet.
    #[arg(short, long, default_value = "world")]
    name: String,

    /// Optional TOML config file to load.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    let config = load_config(args.config).await?;

    info!(prefix = %config.greeting_prefix, name = %args.name, "running cli-app");
    println!("{}{}", config.greeting_prefix, greeting(&args.name));
    Ok(())
}

/// Loads configuration from `path`, falling back to [`AppConfig::default`].
async fn load_config(path: Option<PathBuf>) -> eyre::Result<AppConfig> {
    let Some(path) = path else {
        return Ok(AppConfig::default());
    };

    let text = tokio::fs::read_to_string(path).await?;
    let config = config::Config::builder()
        .add_source(config::File::from_str(&text, config::FileFormat::Toml))
        .build()?;
    Ok(config.try_deserialize()?)
}

/// Runtime configuration loaded from an optional TOML file.
#[derive(Debug, Default, serde::Deserialize)]
struct AppConfig {
    /// Prefix prepended to greetings.
    #[serde(default)]
    greeting_prefix: String,
}

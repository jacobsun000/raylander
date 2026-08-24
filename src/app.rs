use std::thread;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    config::{self, Config},
    web_server::WebServer,
};

#[derive(Debug, Parser)]
#[command(name = "raylander", version, about = "External dock server")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the external dock server, or do nothing if it is already running.
    Daemon,
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("raylander=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let Cli {
        command: Command::Daemon,
    } = Cli::parse();
    let config = config::load().unwrap_or_else(|error| {
        tracing::error!(%error, "invalid configuration; using defaults");
        Config::default()
    });
    anyhow::ensure!(
        config.components.external_dock.enabled,
        "external dock is disabled in configuration"
    );
    let _web_server =
        WebServer::start(&config.components).context("external dock server unavailable")?;

    loop {
        thread::park();
    }
}

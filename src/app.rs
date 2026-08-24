use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
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
    /// Notify connected docks that the display power state changed.
    DisplayPower,
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

    let cli = Cli::parse();
    let config = config::load().unwrap_or_else(|error| {
        tracing::error!(%error, "invalid configuration; using defaults");
        Config::default()
    });
    anyhow::ensure!(
        config.components.external_dock.enabled,
        "external dock is disabled in configuration"
    );
    match cli.command {
        Command::Daemon => {
            let _web_server =
                WebServer::start(&config.components).context("external dock server unavailable")?;
            loop {
                thread::park();
            }
        }
        Command::DisplayPower => notify_display_power(&config.components.external_dock),
    }
}

fn notify_display_power(config: &crate::config::ExternalDockConfig) -> Result<()> {
    let host = match config.bind_address {
        IpAddr::V4(address) if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    };
    let address = SocketAddr::new(host, config.port);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .with_context(|| format!("failed to connect to external dock server at {address}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(
        b"POST /display-power HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 204") {
        bail!("external dock server rejected display-power notification");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_power_command() {
        let cli = Cli::try_parse_from(["raylander", "display-power"]).unwrap();
        assert!(matches!(cli.command, Command::DisplayPower));
    }
}

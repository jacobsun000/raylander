use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::{
    config::{self, Config, ExternalDockConfig},
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
    /// Start the external dock server.
    Daemon,
    /// Notify connected docks that the display power state changed.
    DisplayPower,
    /// Forward one Codex lifecycle hook event from standard input to the daemon.
    #[command(alias = "codex-hook-notify", hide = true)]
    CodexHook,
    /// Manage the Codex Desktop lifecycle hooks.
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum HooksCommand {
    /// Install or repair Raylander's Codex hooks.
    Install,
    /// Remove only the hooks managed by Raylander.
    Remove,
    /// Show whether Raylander's hooks are installed and current.
    Status,
}

pub(crate) struct RunningDaemon {
    _web_server: WebServer,
}

pub fn run() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => {
            let (_daemon, _) = start_daemon(true)?;
            loop {
                thread::park();
            }
        }
        Command::DisplayPower => {
            let config = load_config();
            ensure_dock_enabled(&config)?;
            notify_display_power(&config.components.external_dock)
        }
        Command::CodexHook => run_codex_hook(),
        Command::Hooks { command } => run_hooks_command(command),
    }
}

pub(crate) fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("raylander=info")),
        )
        .with_target(false)
        .compact()
        .try_init();
}

pub(crate) fn start_daemon(automatic_hooks: bool) -> Result<(RunningDaemon, String)> {
    let config = load_config();
    ensure_dock_enabled(&config)?;
    let dock_url = dock_url(&config.components.external_dock);
    let web_server =
        WebServer::start(&config.components).context("external dock server unavailable")?;
    maybe_install_hooks(
        automatic_hooks,
        config.components.agents.enabled,
        local_address(&config.components.external_dock)
            .ip()
            .is_loopback(),
    );
    Ok((
        RunningDaemon {
            _web_server: web_server,
        },
        dock_url,
    ))
}

#[cfg(target_os = "windows")]
pub(crate) fn configured_dock_url() -> Result<String> {
    let config = load_config();
    ensure_dock_enabled(&config)?;
    Ok(dock_url(&config.components.external_dock))
}

fn load_config() -> Config {
    config::load().unwrap_or_else(|error| {
        tracing::error!(%error, "invalid configuration; using defaults");
        Config::default()
    })
}

fn ensure_dock_enabled(config: &Config) -> Result<()> {
    anyhow::ensure!(
        config.components.external_dock.enabled,
        "external dock is disabled in configuration"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn maybe_install_hooks(
    automatic_hooks: bool,
    agents_enabled: bool,
    hook_endpoint_is_loopback: bool,
) {
    if !automatic_hooks || !agents_enabled {
        return;
    }
    if !hook_endpoint_is_loopback {
        tracing::warn!(
            "Codex hooks were not installed automatically because the configured dock address is not loopback-reachable"
        );
        return;
    }
    match install_hooks() {
        Ok(message) => tracing::info!(%message, "Codex hooks checked"),
        Err(error) => {
            tracing::warn!(%error, "Codex hooks could not be installed; daemon remains available")
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn maybe_install_hooks(
    _automatic_hooks: bool,
    _agents_enabled: bool,
    _hook_endpoint_is_loopback: bool,
) {
}

pub(crate) fn install_hooks() -> Result<String> {
    let status = crate::codex_hooks::install()?;
    Ok(format_hook_status("Codex hooks", &status))
}

fn run_codex_hook() -> Result<()> {
    let config = load_config();
    if !config.components.external_dock.enabled || !config.components.agents.enabled {
        return Ok(());
    }
    if let Err(error) =
        crate::codex_hooks::forward_stdin_to_daemon(local_address(&config.components.external_dock))
    {
        // Lifecycle notifications are advisory. In particular, stale installed hooks must not
        // make Codex operations fail just because the Raylander daemon is not running.
        tracing::debug!(%error, "Codex hook notification was not delivered");
    }
    Ok(())
}

fn run_hooks_command(command: HooksCommand) -> Result<()> {
    let message = match command {
        HooksCommand::Install => install_hooks()?,
        HooksCommand::Remove => {
            let status = crate::codex_hooks::uninstall()?;
            format_hook_status("Codex hooks", &status)
        }
        HooksCommand::Status => {
            let status = crate::codex_hooks::status()?;
            format_hook_status("Codex hooks", &status)
        }
    };
    println!("{message}");
    Ok(())
}

fn format_hook_status(label: &str, status: &crate::codex_hooks::HookStatus) -> String {
    let mut message = format!("{label}: {:?}", status.state);
    if let Some(path) = &status.hooks_path {
        message.push_str(&format!(" ({})", path.display()));
    }
    if !status.missing_events.is_empty() {
        message.push_str(&format!("; missing: {}", status.missing_events.join(", ")));
    }
    if !status.stale_events.is_empty() {
        message.push_str(&format!("; stale: {}", status.stale_events.join(", ")));
    }
    if let Some(detail) = &status.detail {
        message.push_str(&format!("; {detail}"));
    }
    message
}

fn dock_url(config: &ExternalDockConfig) -> String {
    format!("http://{}/", local_address(config))
}

fn local_address(config: &ExternalDockConfig) -> SocketAddr {
    let host = match config.bind_address {
        IpAddr::V4(address) if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    };
    SocketAddr::new(host, config.port)
}

fn notify_display_power(config: &ExternalDockConfig) -> Result<()> {
    let address = local_address(config);
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

    #[test]
    fn parses_codex_hook_command() {
        let cli = Cli::try_parse_from(["raylander", "codex-hook"]).unwrap();
        assert!(matches!(cli.command, Command::CodexHook));
    }

    #[test]
    fn parses_hooks_status_command() {
        let cli = Cli::try_parse_from(["raylander", "hooks", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hooks {
                command: HooksCommand::Status
            }
        ));
    }

    #[test]
    fn dock_url_uses_loopback_for_unspecified_bind_address() {
        let config = ExternalDockConfig::default();
        assert_eq!(dock_url(&config), "http://127.0.0.1:8030/");
    }

    #[test]
    fn dock_url_brackets_ipv6_addresses() {
        let config = ExternalDockConfig {
            bind_address: "::".parse().unwrap(),
            port: 8030,
            ..ExternalDockConfig::default()
        };
        assert_eq!(dock_url(&config), "http://[::1]:8030/");
    }
}

use clap::{Parser, Subcommand};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daemon_command() {
        let cli = Cli::try_parse_from(["raylander", "daemon"]).unwrap();
        assert!(matches!(cli.command, Command::Daemon));
    }

    #[test]
    fn rejects_removed_commands() {
        for command in [
            "volume", "mic", "sidebar", "media", "preview", "reload", "quit",
        ] {
            assert!(Cli::try_parse_from(["raylander", command]).is_err());
        }
    }
}

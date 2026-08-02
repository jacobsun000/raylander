use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "raylander", version, about = "Wayland desktop utilities")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the background process, or do nothing if it is already running.
    Daemon,
    /// Control and display the default output volume.
    Volume {
        #[command(subcommand)]
        command: VolumeCommand,
    },
    /// Reload configuration and theme files.
    Reload,
    /// Stop the running background process.
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum VolumeCommand {
    Up,
    Down,
    Mute,
    Show,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_command() {
        let cli = Cli::try_parse_from(["raylander", "volume", "up"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Volume {
                command: VolumeCommand::Up
            }
        ));
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(Cli::try_parse_from(["raylander", "brightness", "up"]).is_err());
    }
}

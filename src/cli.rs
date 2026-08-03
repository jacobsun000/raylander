use std::{path::PathBuf, time::Duration};

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
    /// Preview every available desktop component for a fixed time.
    Demo {
        /// How long the preview remains visible (for example: 500ms, 5s, 2m).
        #[arg(short = 't', long, default_value = "5s", value_parser = parse_duration)]
        duration: Duration,
        /// Render the preview to a PNG instead of displaying it.
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
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

fn parse_duration(value: &str) -> Result<Duration, String> {
    let duration = humantime::parse_duration(value).map_err(|error| error.to_string())?;
    if duration.is_zero() {
        return Err("duration must be greater than zero".into());
    }
    Ok(duration)
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

    #[test]
    fn parses_demo_duration() {
        let cli = Cli::try_parse_from(["raylander", "demo", "-t", "5s"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Demo {
                duration,
                output: None
            } if duration == Duration::from_secs(5)
        ));
    }

    #[test]
    fn demo_defaults_to_five_seconds() {
        let cli = Cli::try_parse_from(["raylander", "demo"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Demo {
                duration,
                output: None
            } if duration == Duration::from_secs(5)
        ));
    }

    #[test]
    fn parses_demo_output() {
        let cli = Cli::try_parse_from(["raylander", "demo", "-o", "preview.png"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Demo {
                output: Some(path),
                ..
            } if path == PathBuf::from("preview.png")
        ));
    }

    #[test]
    fn rejects_zero_demo_duration() {
        assert!(Cli::try_parse_from(["raylander", "demo", "-t", "0s"]).is_err());
    }
}

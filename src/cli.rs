use std::{path::PathBuf, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};

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
    /// Control and display the default microphone level.
    Mic {
        #[command(subcommand)]
        command: MicCommand,
    },
    /// Show or hide the unified desktop sidebar.
    Sidebar {
        #[command(subcommand)]
        command: SidebarCommand,
    },
    /// Manage notification history and do-not-disturb mode.
    Notifications {
        #[command(subcommand)]
        command: NotificationsCommand,
    },
    /// Control the current MPRIS media player.
    Media {
        #[command(subcommand)]
        command: MediaCommand,
    },
    /// Control the Fcitx5 input method.
    Input {
        #[command(subcommand)]
        command: InputCommand,
    },
    /// Refresh package update status.
    Packages {
        #[command(subcommand)]
        command: PackagesCommand,
    },
    /// Preview current build without connecting to the running daemon.
    Preview {
        /// How long the preview remains visible (for example: 500ms, 5s, 2m).
        #[arg(short = 't', long, default_value = "5s", value_parser = parse_duration)]
        duration: Duration,
        /// Render the preview to a PNG instead of displaying it.
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Override the configured appearance for this preview.
        #[arg(long, value_enum)]
        color_scheme: Option<PreviewColorScheme>,
    },
    /// Notify connected docks that the display power state changed.
    DisplayPower,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum MicCommand {
    Up,
    Down,
    Mute,
    Show,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum SidebarCommand {
    Show {
        #[arg(value_enum)]
        section: Option<SidebarSection>,
    },
    Hide,
    Toggle {
        #[arg(value_enum)]
        section: Option<SidebarSection>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SidebarSection {
    Controls,
    Playing,
    Calendar,
    Packages,
    Notifications,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum NotificationsCommand {
    Clear,
    Dnd {
        #[arg(value_enum)]
        state: ToggleState,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ToggleState {
    On,
    Off,
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum InputCommand {
    Next,
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum PackagesCommand {
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PreviewColorScheme {
    Light,
    Dark,
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
    fn parses_display_power_notification() {
        let cli = Cli::try_parse_from(["raylander", "display-power"]).unwrap();
        assert!(matches!(cli.command, Command::DisplayPower));
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(Cli::try_parse_from(["raylander", "brightness", "up"]).is_err());
    }

    #[test]
    fn parses_standalone_preview() {
        let cli =
            Cli::try_parse_from(["raylander", "preview", "-t", "2s", "--color-scheme", "dark"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Command::Preview {
                duration,
                output: None,
                color_scheme: Some(PreviewColorScheme::Dark),
            } if duration == Duration::from_secs(2)
        ));
    }

    #[test]
    fn parses_sidebar_target() {
        let cli = Cli::try_parse_from(["raylander", "sidebar", "toggle", "notifications"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Sidebar {
                command: SidebarCommand::Toggle {
                    section: Some(SidebarSection::Notifications)
                }
            }
        ));
    }

    #[test]
    fn parses_microphone_command() {
        let cli = Cli::try_parse_from(["raylander", "mic", "mute"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Mic {
                command: MicCommand::Mute
            }
        ));
    }

    #[test]
    fn parses_notification_dnd_command() {
        let cli = Cli::try_parse_from(["raylander", "notifications", "dnd", "toggle"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Notifications {
                command: NotificationsCommand::Dnd {
                    state: ToggleState::Toggle
                }
            }
        ));
    }

    #[test]
    fn parses_media_and_package_commands() {
        assert!(matches!(
            Cli::try_parse_from(["raylander", "media", "play-pause"])
                .unwrap()
                .command,
            Command::Media {
                command: MediaCommand::PlayPause
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["raylander", "packages", "refresh"])
                .unwrap()
                .command,
            Command::Packages {
                command: PackagesCommand::Refresh
            }
        ));
    }
}

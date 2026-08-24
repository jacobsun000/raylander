use std::process::Command;

use anyhow::{Context, Result, bail};

const FIELD_SEPARATOR: char = '\u{1f}';
const PLAYER_FORMAT: &str = "{{playerName}}\u{1f}{{status}}\u{1f}{{xesam:title}}\u{1f}{{xesam:artist}}\u{1f}{{xesam:album}}\u{1f}{{mpris:artUrl}}\u{1f}{{position}}\u{1f}{{mpris:length}}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaState {
    pub player: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub position_micros: Option<u64>,
    pub duration_micros: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    Play,
    Pause,
    Next,
    Previous,
}

pub struct PlayerctlBackend;

impl PlayerctlBackend {
    pub fn load_artwork(&self, url: &str) -> Result<Option<Vec<u8>>> {
        if let Some(path) = url.strip_prefix("file://") {
            return std::fs::read(path)
                .with_context(|| format!("failed to read media artwork {path}"))
                .map(Some);
        }
        if url.starts_with("https://") || url.starts_with("http://") {
            let output = Command::new("curl")
                .args(["--fail", "--silent", "--max-time", "5", url])
                .output()
                .with_context(|| format!("failed to fetch media artwork {url}"))?;
            if !output.status.success() {
                bail!("media artwork request exited with {}", output.status);
            }
            return Ok(Some(output.stdout));
        }
        Ok(None)
    }

    /// Returns the playing player, falling back to a paused and then stopped player.
    pub fn current(&self) -> Result<Option<MediaState>> {
        let output = Command::new("playerctl")
            .args(["--all-players", "metadata", "--format", PLAYER_FORMAT])
            .output()
            .context("failed to execute playerctl metadata")?;

        // playerctl exits non-zero when no compatible player is available.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("No players found") || stderr.contains("No player could handle") {
                return Ok(None);
            }
            bail!(
                "playerctl metadata exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }

        let stdout =
            String::from_utf8(output.stdout).context("playerctl returned non-UTF-8 metadata")?;
        Ok(select_current_player(&parse_playerctl_metadata(&stdout)?).cloned())
    }

    pub fn apply(&self, player: Option<&str>, action: MediaAction) -> Result<()> {
        let mut command = Command::new("playerctl");
        if let Some(player) = player {
            command.args(["--player", player]);
        }
        match action {
            MediaAction::Play => command.arg("play"),
            MediaAction::Pause => command.arg("pause"),
            MediaAction::Next => command.arg("next"),
            MediaAction::Previous => command.arg("previous"),
        };
        let status = command
            .status()
            .context("failed to execute playerctl action")?;
        if !status.success() {
            bail!("playerctl action exited with {status}");
        }
        Ok(())
    }
}

pub fn parse_playerctl_metadata(output: &str) -> Result<Vec<MediaState>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_player)
        .collect()
}

pub fn select_current_player(players: &[MediaState]) -> Option<&MediaState> {
    [
        PlaybackStatus::Playing,
        PlaybackStatus::Paused,
        PlaybackStatus::Stopped,
    ]
    .into_iter()
    .find_map(|status| players.iter().find(|player| player.status == status))
}

fn parse_player(line: &str) -> Result<MediaState> {
    let fields: Vec<_> = line.split(FIELD_SEPARATOR).collect();
    if fields.len() != 8 {
        bail!(
            "playerctl metadata contained {} fields; expected 8",
            fields.len()
        );
    }
    let status = match fields[1] {
        "Playing" => PlaybackStatus::Playing,
        "Paused" => PlaybackStatus::Paused,
        "Stopped" => PlaybackStatus::Stopped,
        other => bail!("playerctl returned unknown playback status {other:?}"),
    };
    if fields[0].is_empty() {
        bail!("playerctl metadata omitted the player name");
    }
    Ok(MediaState {
        player: fields[0].into(),
        status,
        title: fields[2].into(),
        artists: fields[3]
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect(),
        album: nonempty(fields[4]),
        artwork_url: nonempty(fields[5]),
        position_micros: parse_optional_u64(fields[6], "position")?,
        duration_micros: parse_optional_u64(fields[7], "duration")?,
    })
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_optional_u64(value: &str, name: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .with_context(|| format!("playerctl returned invalid {name} {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_and_sparse_players() {
        let source = "spotify\u{1f}Playing\u{1f}Song\u{1f}Ada, Bob\u{1f}Album\u{1f}file:///cover.jpg\u{1f}1200000\u{1f}3000000\nmpv\u{1f}Paused\u{1f}Video\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}\n";
        let players = parse_playerctl_metadata(source).unwrap();
        assert_eq!(players.len(), 2);
        assert_eq!(players[0].artists, ["Ada", "Bob"]);
        assert_eq!(players[0].position_micros, Some(1_200_000));
        assert_eq!(players[1].album, None);
        assert_eq!(players[1].duration_micros, None);
    }

    #[test]
    fn selects_playing_then_paused_then_stopped() {
        let source = "one\u{1f}Stopped\u{1f}A\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}\ntwo\u{1f}Paused\u{1f}B\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}\nthree\u{1f}Playing\u{1f}C\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}\n";
        let players = parse_playerctl_metadata(source).unwrap();
        assert_eq!(select_current_player(&players).unwrap().player, "three");
        assert_eq!(select_current_player(&players[..2]).unwrap().player, "two");
        assert_eq!(select_current_player(&players[..1]).unwrap().player, "one");
        assert!(select_current_player(&[]).is_none());
    }

    #[test]
    fn rejects_bad_wire_data() {
        assert!(
            parse_playerctl_metadata(
                "spotify\u{1f}Buffering\u{1f}Song\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}"
            )
            .is_err()
        );
        assert!(parse_playerctl_metadata("spotify\u{1f}Playing").is_err());
        assert!(
            parse_playerctl_metadata(
                "spotify\u{1f}Playing\u{1f}Song\u{1f}\u{1f}\u{1f}\u{1f}nope\u{1f}1"
            )
            .is_err()
        );
    }
}

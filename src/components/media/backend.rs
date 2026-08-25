#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
use anyhow::bail;
use anyhow::{Context, Result};

#[cfg(target_os = "linux")]
const FIELD_SEPARATOR: char = '\u{1f}';
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    pub fn load_artwork(&self, url: &str) -> Result<Option<Vec<u8>>> {
        if let Some(path) = url.strip_prefix("file://") {
            return std::fs::read(path)
                .with_context(|| format!("failed to read media artwork {path}"))
                .map(Some);
        }
        if url.starts_with("https://") || url.starts_with("http://") {
            let response = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .context("failed to build media artwork client")?
                .get(url)
                .send()
                .with_context(|| format!("failed to fetch media artwork {url}"))?
                .error_for_status()
                .with_context(|| format!("media artwork request failed for {url}"))?;
            return response
                .bytes()
                .map(|bytes| Some(bytes.to_vec()))
                .context("failed to read media artwork response");
        }
        Ok(None)
    }

    /// Returns the playing player, falling back to a paused and then stopped player.
    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "windows")]
    pub fn current(&self) -> Result<Option<MediaState>> {
        use windows::Media::Control::{
            GlobalSystemMediaTransportControlsSessionManager as Manager,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
        };

        let manager = Manager::RequestAsync()?.join()?;
        let Some(session) = manager.GetCurrentSession().ok() else {
            return Ok(None);
        };
        let properties = session.TryGetMediaPropertiesAsync()?.join()?;
        let playback = session.GetPlaybackInfo()?;
        let timeline = session.GetTimelineProperties().ok();
        let player = session.SourceAppUserModelId()?.to_string_lossy();
        let title = properties.Title()?.to_string_lossy();
        let artist = properties.Artist()?.to_string_lossy();
        let album = properties.AlbumTitle()?.to_string_lossy();
        let has_thumbnail = properties.Thumbnail().is_ok();
        let status = match playback.PlaybackStatus()? {
            Status::Playing => PlaybackStatus::Playing,
            Status::Paused => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        };
        Ok(Some(MediaState {
            artwork_url: has_thumbnail.then(|| format!("gsmtc://{player}")),
            player,
            status,
            title,
            artists: (!artist.is_empty()).then_some(artist).into_iter().collect(),
            album: (!album.is_empty()).then_some(album),
            position_micros: timeline
                .as_ref()
                .map(|value| (value.Position().unwrap_or_default().Duration.max(0) as u64) / 10),
            duration_micros: timeline
                .as_ref()
                .map(|value| (value.EndTime().unwrap_or_default().Duration.max(0) as u64) / 10),
        }))
    }

    #[cfg(target_os = "windows")]
    pub fn apply(&self, _player: Option<&str>, action: MediaAction) -> Result<()> {
        use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Manager;

        let manager = Manager::RequestAsync()?.join()?;
        let session = manager
            .GetCurrentSession()
            .context("no active Windows media session")?;
        match action {
            MediaAction::Play => session.TryPlayAsync()?.join()?,
            MediaAction::Pause => session.TryPauseAsync()?.join()?,
            MediaAction::Next => session.TrySkipNextAsync()?.join()?,
            MediaAction::Previous => session.TrySkipPreviousAsync()?.join()?,
        };
        Ok(())
    }

    #[cfg(target_os = "windows")]
    pub fn load_artwork(&self, url: &str) -> Result<Option<Vec<u8>>> {
        use windows::{
            Media::Control::GlobalSystemMediaTransportControlsSessionManager as Manager,
            Storage::Streams::DataReader,
        };

        if !url.starts_with("gsmtc://") {
            return Ok(None);
        }
        let manager = Manager::RequestAsync()?.join()?;
        let session = manager
            .GetCurrentSession()
            .context("no active Windows media session")?;
        let properties = session.TryGetMediaPropertiesAsync()?.join()?;
        let stream = properties.Thumbnail()?.OpenReadAsync()?.join()?;
        let size = stream.Size()?;
        let size = u32::try_from(size).context("media thumbnail is too large")?;
        let reader = DataReader::CreateDataReader(&stream)?;
        reader.LoadAsync(size)?.join()?;
        let mut bytes = vec![0; size as usize];
        reader.ReadBytes(&mut bytes)?;
        Ok(Some(bytes))
    }
}

#[cfg(target_os = "linux")]
pub fn parse_playerctl_metadata(output: &str) -> Result<Vec<MediaState>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_player)
        .collect()
}

#[cfg(target_os = "linux")]
pub fn select_current_player(players: &[MediaState]) -> Option<&MediaState> {
    [
        PlaybackStatus::Playing,
        PlaybackStatus::Paused,
        PlaybackStatus::Stopped,
    ]
    .into_iter()
    .find_map(|status| players.iter().find(|player| player.status == status))
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(target_os = "linux")]
fn parse_optional_u64(value: &str, name: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .with_context(|| format!("playerctl returned invalid {name} {value:?}"))
}

#[cfg(all(test, target_os = "linux"))]
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

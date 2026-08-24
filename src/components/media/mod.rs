use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use axum::{
    Router,
    extract::{Path, Query},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use crate::components::GridSize;

use self::backend::{MediaAction, MediaState, PlaybackStatus, PlayerctlBackend};

use super::dock_component;

pub(crate) mod backend;
pub(crate) mod visualizer;

dock_component!(Media, "media", GridSize::ONE_BY_ONE, routes());

static SPECTRUM: OnceLock<Mutex<[u8; 16]>> = OnceLock::new();
static SPECTRUM_ACTIVE: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static ARTWORK_CACHE: OnceLock<Mutex<HashMap<u64, Vec<u8>>>> = OnceLock::new();

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/component.js", get(component_javascript))
        .route("/arch-logo.png", get(arch_logo))
        .route("/action/{action}", post(media_action))
        .route("/artwork", get(artwork))
        .route("/visualizer/{active}", post(set_visualizer_active))
}

async fn arch_logo() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("../../../assets/web/arch-logo.png").as_slice(),
    )
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}

pub(crate) fn observe_media(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    let events = crate::components::native_observers::observe_media();
    thread::Builder::new()
        .name("raylander-media".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                if events
                    .recv_timeout(std::time::Duration::from_millis(250))
                    .is_ok()
                {
                    on_change();
                }
            }
        })
        .expect("failed to start media observer")
}

pub(crate) fn observe_spectrum(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    let events = crate::components::media::visualizer::observe(visualizer_active());
    thread::Builder::new()
        .name("raylander-spectrum".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                if let Ok(levels) = events.recv_timeout(std::time::Duration::from_millis(250)) {
                    set_spectrum(levels);
                    on_change();
                }
            }
        })
        .expect("failed to start spectrum observer")
}

pub(crate) fn visualizer_active() -> Arc<AtomicBool> {
    Arc::clone(SPECTRUM_ACTIVE.get_or_init(|| Arc::new(AtomicBool::new(false))))
}

pub(crate) fn set_spectrum(levels: [u8; 16]) {
    *SPECTRUM
        .get_or_init(|| Mutex::new([0; 16]))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = levels;
}

pub(crate) fn spectrum() -> [u8; 16] {
    *SPECTRUM
        .get_or_init(|| Mutex::new([0; 16]))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

#[derive(Deserialize)]
struct ArtworkQuery {
    t: Option<u64>,
}

async fn artwork(Query(query): Query<ArtworkQuery>) -> Response {
    let bytes = query.t.and_then(|key| {
        ARTWORK_CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .cloned()
    });
    match bytes {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/*"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) fn artwork_key(media: &MediaState) -> u64 {
    let mut hasher = DefaultHasher::new();
    media.player.hash(&mut hasher);
    media.title.hash(&mut hasher);
    media.artwork_url.hash(&mut hasher);
    let key = hasher.finish();
    if let Some(url) = media.artwork_url.as_deref() {
        let cache = ARTWORK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let missing = !cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&key);
        if missing && let Some(bytes) = load_artwork(url) {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            if cache.len() >= 12 {
                cache.clear();
            }
            cache.insert(key, bytes);
        }
    }
    key
}

fn load_artwork(url: &str) -> Option<Vec<u8>> {
    PlayerctlBackend.load_artwork(url).ok().flatten()
}

async fn set_visualizer_active(Path(active): Path<bool>) -> StatusCode {
    visualizer_active().store(active, Ordering::Relaxed);
    if !active {
        set_spectrum([0; 16]);
    }
    StatusCode::NO_CONTENT
}

async fn media_action(Path(action): Path<String>) -> StatusCode {
    let action = match action.as_str() {
        "previous" => MediaAction::Previous,
        "play" => MediaAction::Play,
        "pause" => MediaAction::Pause,
        "next" => MediaAction::Next,
        _ => return StatusCode::NOT_FOUND,
    };
    match tokio::task::spawn_blocking(move || PlayerctlBackend.apply(None, action)).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(crate) fn spectrum_fragment() -> String {
    let levels = spectrum();
    let levels = levels
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"<div id="spectrum-data" data-levels="{levels}" hidden hx-swap-oob="outerHTML"></div>"#
    )
}

fn media_primary_control(status: PlaybackStatus) -> &'static str {
    if status == PlaybackStatus::Playing {
        r#"<button class="media-primary playing" onclick="fetch('/components/media/action/pause',{method:'POST'})" aria-label="Pause"><svg class="play-icon" viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg><svg class="pause-icon" viewBox="0 0 24 24"><path d="M7 5h4v14H7zM13 5h4v14h-4z"/></svg></button>"#
    } else {
        r#"<button class="media-primary" onclick="fetch('/components/media/action/play',{method:'POST'})" aria-label="Play"><svg class="play-icon" viewBox="0 0 24 24"><path d="m8 5 11 7-11 7z"/></svg><svg class="pause-icon" viewBox="0 0 24 24"><path d="M7 5h4v14H7zM13 5h4v14h-4z"/></svg></button>"#
    }
}

fn media_fullscreen(media: &crate::components::media::backend::MediaState) -> String {
    let art_key = artwork_key(media);
    let primary = media_primary_control(media.status);
    let progress = match (media.position_micros, media.duration_micros) {
        (Some(position), Some(duration)) if duration > 0 => {
            (position.saturating_mul(100) / duration).min(100)
        }
        _ => 0,
    };
    let playing = if media.status == PlaybackStatus::Playing {
        " playing"
    } else {
        ""
    };
    format!(
        r#"<div id="media-full-panel" class="media-full-panel no-lyrics{playing}" hx-swap-oob="outerHTML"><div class="media-full-ambient"><i></i><i></i><i></i></div><div id="media-visualizer" class="media-visualizer"><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i><i></i></div><section class="media-full-left"><img src="/components/media/artwork?t={art_key}" alt="Album artwork"><h2>{}</h2><p>{}</p><div class="media-progress-wrap"><div class="media-progress"><i style="width:{progress}%"></i></div></div><div class="media-full-controls"><button onclick="fetch('/components/media/action/previous',{{method:'POST'}})"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button>{primary}<button onclick="fetch('/components/media/action/next',{{method:'POST'}})"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></section></div>"#,
        escape(&media.title),
        escape(&media.artists.join(", "))
    )
}

pub(crate) fn update_fragment() -> String {
    let mut html = String::new();
    match PlayerctlBackend.current() {
        Ok(Some(media)) => {
            let playing = if media.status == PlaybackStatus::Playing { " playing" } else { "" };
            let primary_action = media_primary_control(media.status);
            html.push_str(&format!(r#"<div id="media-ambient" class="media-ambient{playing}" hx-swap-oob="outerHTML"><i></i><i></i><i></i></div>"#));
            html.push_str(&format!(r#"<div id="media-empty" class="media-empty hidden" hx-swap-oob="outerHTML"></div><div id="media-copy" class="media-copy" hx-swap-oob="outerHTML"><p id="media-title" class="value truncate">{}</p><p id="media-artist" class="truncate text-[9px] text-[#a6acb9]">{}</p><div class="media-controls" onclick="event.stopPropagation()"><button onclick="fetch('/components/media/action/previous',{{method:'POST'}})" aria-label="Previous"><svg viewBox="0 0 24 24"><path d="M6 5h2v14H6zm3 7 9 7V5z"/></svg></button>{primary_action}<button onclick="fetch('/components/media/action/next',{{method:'POST'}})" aria-label="Next"><svg viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM6 5l9 7-9 7z"/></svg></button></div></div>"#, escape(&media.title), escape(&media.artists.join(", "))));
            if media.artwork_url.is_some() { html.push_str(&format!(r#"<img id="media-art" src="/components/media/artwork?t={}" alt="" hx-swap-oob="outerHTML" onerror="this.hidden=true" onload="this.hidden=false">"#, artwork_key(&media))); }
            html.push_str(&media_fullscreen(&media));
        }
        _ => html.push_str(r#"<div id="media-ambient" class="media-ambient" hx-swap-oob="outerHTML"></div><img id="media-art" hidden hx-swap-oob="outerHTML" alt=""><div id="media-empty" class="media-empty" hx-swap-oob="outerHTML" aria-hidden="true"><img src="/components/media/arch-logo.png" alt=""></div><div id="media-copy" class="media-copy hidden" hx-swap-oob="outerHTML"></div>"#),
    }
    html
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

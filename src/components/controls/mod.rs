use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};

use crate::components::GridSize;

use self::{audio::WpctlBackend, display::DdcutilBackend};

use super::dock_component;

pub(crate) mod audio;
pub(crate) mod display;

dock_component!(Controls, "controls", GridSize::ONE_BY_ONE, routes());

static DOCK_DISPLAY: OnceLock<u32> = OnceLock::new();
static LAST_VOLUME: OnceLock<Mutex<Option<u16>>> = OnceLock::new();
static LAST_BRIGHTNESS: OnceLock<Mutex<Option<u8>>> = OnceLock::new();
static BRIGHTNESS: OnceLock<Mutex<Option<(Instant, String)>>> = OnceLock::new();

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/component.js", get(component_javascript))
        .route("/volume/{percent}", post(set_volume))
        .route("/brightness/{percent}", post(set_brightness))
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}

pub(crate) fn observe_volume(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    let events = crate::components::native_observers::observe_volume();
    thread::Builder::new()
        .name("raylander-volume".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                if events.recv_timeout(Duration::from_millis(250)).is_ok() {
                    on_change();
                }
            }
        })
        .expect("failed to start volume observer")
}

pub(crate) fn volume_fragment() -> String {
    let Ok(volume) = WpctlBackend.current() else {
        return String::new();
    };
    let percent = if volume.muted {
        0
    } else {
        volume.percent.min(100)
    };
    let mut html = String::new();
    flash_if_changed(&LAST_VOLUME, &mut html, "volume", percent);
    html.push_str(&format!(r#"<div id="volume-fill" class="ios-fill bg-[#f1f2f5]" hx-swap-oob="outerHTML" style="height:{percent}%"></div>"#));
    html
}

pub(crate) fn update_fragment() -> String {
    let mut html = display_power_fragment();
    html.push_str(&volume_fragment());
    let mut cache = BRIGHTNESS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache
        .as_ref()
        .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(5 * 60))
        && let Some(fragment) = brightness_fragment()
    {
        *cache = Some((Instant::now(), fragment));
    }
    if let Some((_, fragment)) = cache.as_ref() {
        html.push_str(fragment);
    }
    html
}

fn brightness_fragment() -> Option<String> {
    let (display, value) = DdcutilBackend
        .all_brightness()
        .ok()?
        .into_iter()
        .find(|(display, _)| is_dock_display(display))?;
    remember_display(display.number);
    let percent = value.percent();
    let mut html = String::new();
    flash_if_changed(&LAST_BRIGHTNESS, &mut html, "brightness", percent);
    html.push_str(&format!(r#"<div id="brightness-fill" class="ios-fill bg-[#f1f2f5]" hx-swap-oob="outerHTML" style="height:{percent}%"></div>"#));
    Some(html)
}

pub(crate) fn display_power_fragment() -> String {
    let enabled = DdcutilBackend.display_power_enabled().unwrap_or(false);
    format!(r#"<div id="display-power" data-on="{enabled}" hidden hx-swap-oob="outerHTML"></div>"#)
}

fn flash_if_changed<T: Copy + PartialEq + std::fmt::Display>(
    cache: &'static OnceLock<Mutex<Option<T>>>,
    html: &mut String,
    id: &str,
    value: T,
) {
    let mut previous = cache
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let changed = previous.is_some_and(|old| old != value);
    *previous = Some(value);
    if changed {
        html.push_str(&format!(
            r#"<span id="{id}" class="gauge-percent flash" hx-swap-oob="outerHTML">{value}%</span>"#
        ));
    }
}

pub(crate) fn display() -> Option<u32> {
    DOCK_DISPLAY.get().copied()
}

pub(crate) fn remember_display(display: u32) {
    let _ = DOCK_DISPLAY.set(display);
}

async fn set_volume(Path(percent): Path<u8>) -> StatusCode {
    if percent > 100 {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || WpctlBackend.set_percent(percent)).await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn set_brightness(Path(percent): Path<u8>) -> StatusCode {
    if percent > 100 {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || {
        let display = display().or_else(|| {
            DdcutilBackend
                .displays()
                .ok()?
                .into_iter()
                .find(is_dock_display)
                .map(|display| {
                    remember_display(display.number);
                    display.number
                })
        });
        display.map(|display| DdcutilBackend.set_brightness(display, percent))
    })
    .await;
    match result {
        Ok(Some(Ok(()))) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn is_dock_display(display: &crate::components::controls::display::DdcDisplay) -> bool {
    #[cfg(target_os = "linux")]
    return display
        .connector
        .as_deref()
        .is_some_and(|connector| connector.ends_with("DP-2"));

    #[cfg(target_os = "windows")]
    {
        let _ = display;
        true
    }
}

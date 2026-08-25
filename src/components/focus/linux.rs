use std::{
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};

use serde::Deserialize;

use crate::components::GridSize;

use self::backend::HyprlandBackend;
use crate::components::dock_component;

#[path = "backend.rs"]
mod backend;

dock_component!(Focus, "focus", GridSize::ONE_BY_ONE, routes());

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/component.js", get(component_javascript))
        .route("/workspace/{id}", post(focus_workspace))
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    id: i32,
    focused: bool,
    active_workspace: HyprWorkspaceRef,
}

#[derive(Deserialize)]
struct HyprWorkspaceRef {
    id: i32,
}

#[derive(Deserialize)]
struct HyprWorkspace {
    id: i32,
    windows: u32,
    #[serde(rename = "lastwindowtitle")]
    last_window_title: String,
}

pub(crate) fn observe(
    shutdown: Arc<AtomicBool>,
    on_workspace: impl Fn() + Send + 'static,
    on_monitor: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-hyprland".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let Some(signature) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") else {
                    thread::sleep(Duration::from_secs(5));
                    continue;
                };
                let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
                let path = std::path::PathBuf::from(runtime)
                    .join("hypr")
                    .join(signature)
                    .join(".socket2.sock");
                let Ok(stream) = UnixStream::connect(path) else {
                    thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                for line in BufReader::new(stream).lines() {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(line) = line else { break };
                    if line.starts_with("workspace") || line.starts_with("focusedmon") {
                        on_workspace();
                    } else if line.starts_with("monitoradded")
                        || line.starts_with("monitorremoved")
                        || line.starts_with("configreloaded")
                    {
                        on_monitor();
                    }
                }
            }
        })
        .expect("failed to start Hyprland observer")
}

pub(crate) fn oob_fragment() -> String {
    update_fragment().map_or_else(String::new, |fragment| {
        format!(r#"<span id="monitors" hx-swap-oob="innerHTML">{fragment}</span>"#)
    })
}

pub(crate) fn update_fragment() -> Option<String> {
    let monitors: Vec<HyprMonitor> = HyprlandBackend.query("monitors").ok()?;
    let workspaces: Vec<HyprWorkspace> = HyprlandBackend.query("workspaces").ok()?;
    let focused_title = monitors
        .iter()
        .find(|monitor| monitor.focused)
        .and_then(|monitor| {
            workspaces
                .iter()
                .find(|workspace| workspace.id == monitor.active_workspace.id)
        })
        .map(|workspace| workspace.last_window_title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Desktop");
    let focused_title = if focused_title.to_ascii_lowercase().contains("firefox") {
        "Firefox"
    } else {
        focused_title
    };
    let monitor_views = monitors
        .into_iter()
        .map(|monitor| {
            let base = monitor.id * 9 + 1;
            let active = monitor.active_workspace.id;
            let dots = [6, 7, 8, 3, 4, 5, 0, 1, 2]
                .into_iter()
                .map(|offset| {
                    let id = base + offset;
                    let occupied = workspaces.iter().any(|workspace| workspace.id == id && workspace.windows > 0);
                    let class = if id == active && monitor.focused { "active focused" } else if id == active { "active" } else if occupied { "occupied" } else { "" };
                    format!(r#"<i class="{class}"></i>"#)
                })
                .collect::<String>();
            format!(r#"<div class="monitor-view" data-base="{base}" data-active="{active}"><div class="workspace-dots">{dots}</div></div>"#)
        })
        .collect::<String>();
    let title = escape(focused_title);
    Some(format!(
        r#"<p class="focus-title" title="{title}">{title}</p><div class="monitor-grid">{monitor_views}</div>"#
    ))
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn focus_workspace(Path(id): Path<i32>) -> StatusCode {
    if !(1..=27).contains(&id) {
        return StatusCode::BAD_REQUEST;
    }
    let result = tokio::task::spawn_blocking(move || HyprlandBackend.focus_workspace(id)).await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

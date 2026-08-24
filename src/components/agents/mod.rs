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

use self::backend::{AgentStatus, HerdrBackend};
use super::dock_component;

pub(crate) mod backend;

dock_component!(Agents, "agents", GridSize::ONE_BY_ONE, routes());

#[derive(Clone, Default)]
struct AgentWidgetState {
    agents: Vec<AgentStatus>,
    completed: Vec<AgentStatus>,
    completed_at: Option<Instant>,
}

type Invalidate = Arc<dyn Fn() + Send + Sync>;
static STATE: OnceLock<Mutex<AgentWidgetState>> = OnceLock::new();
static INVALIDATE: OnceLock<Invalidate> = OnceLock::new();

fn routes() -> Router {
    Router::new()
        .route("/component.js", get(component_javascript))
        .route("/pi-logo.svg", get(pi_logo))
        .route("/focus", post(focus_agent))
        .route("/focus/{pane_id}", post(focus_agent_by_id))
}

async fn pi_logo() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        include_str!("../../../assets/web/pi-logo.svg"),
    )
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}

fn snapshot() -> AgentWidgetState {
    STATE
        .get_or_init(|| Mutex::new(AgentWidgetState::default()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn invalidate() {
    if let Some(invalidate) = INVALIDATE.get() {
        invalidate();
    }
}

pub(crate) fn observe(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + Sync + 'static,
) -> thread::JoinHandle<()> {
    let _ = INVALIDATE.set(Arc::new(on_change));
    thread::Builder::new()
        .name("raylander-agents".into())
        .spawn(move || {
            let mut previous = Vec::<AgentStatus>::new();
            while !shutdown.load(Ordering::Relaxed) {
                if let Ok(current) = HerdrBackend.agents()
                    && current != previous
                {
                    let completed = current
                        .iter()
                        .filter(|agent| {
                            agent.status == "done"
                                || (agent.status == "idle"
                                    && previous.iter().any(|old| {
                                        old.workspace == agent.workspace
                                            && old.pane == agent.pane
                                            && old.status == "working"
                                    }))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    let mut state = STATE
                        .get_or_init(|| Mutex::new(AgentWidgetState::default()))
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    state.agents.clone_from(&current);
                    for agent in completed {
                        if !state
                            .completed
                            .iter()
                            .any(|completed| completed.pane_id == agent.pane_id)
                        {
                            state.completed.push(agent);
                        }
                    }
                    if !state.completed.is_empty() {
                        state.completed_at = Some(Instant::now());
                    }
                    previous = current;
                    drop(state);
                    invalidate();
                }
                let mut state = STATE
                    .get_or_init(|| Mutex::new(AgentWidgetState::default()))
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if state
                    .completed_at
                    .is_some_and(|at| at.elapsed() >= Duration::from_secs(10))
                {
                    state.completed.clear();
                    state.completed_at = None;
                    drop(state);
                    invalidate();
                }
                thread::sleep(Duration::from_millis(500));
            }
        })
        .expect("failed to start agents observer")
}

async fn focus_agent_by_id(Path(pane_id): Path<String>) -> StatusCode {
    let focus_id = pane_id.clone();
    match tokio::task::spawn_blocking(move || HerdrBackend.focus(&focus_id)).await {
        Ok(Ok(())) => {
            let mut state = STATE
                .get_or_init(|| Mutex::new(AgentWidgetState::default()))
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.completed.retain(|agent| agent.pane_id != pane_id);
            if let Some(agent) = state
                .agents
                .iter_mut()
                .find(|agent| agent.pane_id == pane_id && agent.status == "done")
            {
                agent.status = "idle".to_owned();
            }
            if state.completed.is_empty() && !state.agents.iter().any(|a| a.status == "done") {
                state.completed_at = None;
            }
            drop(state);
            invalidate();
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn focus_agent() -> StatusCode {
    let pane_id = {
        let mut state = STATE
            .get_or_init(|| Mutex::new(AgentWidgetState::default()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let pane_id = state
            .agents
            .iter()
            .find(|agent| agent.status == "blocked")
            .or_else(|| state.agents.iter().find(|agent| agent.status == "done"))
            .or_else(|| state.completed.first())
            .map(|agent| agent.pane_id.clone());
        if let Some(pane_id) = &pane_id {
            state.completed.retain(|agent| &agent.pane_id != pane_id);
            if let Some(agent) = state.agents.iter_mut().find(|a| &a.pane_id == pane_id) {
                agent.status = "idle".to_owned();
            }
        }
        pane_id
    };
    let Some(pane_id) = pane_id else {
        return StatusCode::NO_CONTENT;
    };
    match tokio::task::spawn_blocking(move || HerdrBackend.focus(&pane_id)).await {
        Ok(Ok(())) => {
            invalidate();
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(crate) fn update_fragment() -> String {
    let state = snapshot();
    let blocked = state
        .agents
        .iter()
        .filter(|agent| agent.status == "blocked")
        .collect::<Vec<_>>();
    let working = state
        .agents
        .iter()
        .filter(|agent| agent.status == "working")
        .collect::<Vec<_>>();
    let mut completed_agents = state.completed.clone();
    for agent in state.agents.iter().filter(|agent| agent.status == "done") {
        if !completed_agents
            .iter()
            .any(|item| item.pane_id == agent.pane_id)
        {
            completed_agents.push(agent.clone());
        }
    }
    let completed = !completed_agents.is_empty()
        && (state.agents.iter().any(|agent| agent.status == "done")
            || state
                .completed_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(10)));
    let (class, title, agents): (&str, &str, Vec<(&str, &str, &str)>) = if !blocked.is_empty() {
        (
            "card agent-card attention",
            "Attention needed",
            blocked
                .iter()
                .map(|a| (a.workspace.as_str(), a.pane.as_str(), "blocked"))
                .collect(),
        )
    } else if completed {
        (
            "card agent-card completed",
            "Agent completed",
            completed_agents
                .iter()
                .map(|agent| (agent.workspace.as_str(), agent.pane.as_str(), "done"))
                .collect(),
        )
    } else if !working.is_empty() {
        (
            "card agent-card running",
            "Agents running",
            working
                .iter()
                .map(|a| (a.workspace.as_str(), a.pane.as_str(), "working"))
                .collect(),
        )
    } else {
        ("card agent-card idle", "Agents idle", Vec::new())
    };
    let rows = agents
        .into_iter()
        .map(|(workspace, pane, status)| {
            let indicator = if status == "working" {
                r#"<svg class="agent-working-icon" viewBox="0 0 24 18" role="img" aria-label="working"><rect x="2" y="7" width="4" height="9" rx="2"/><rect x="10" y="2" width="4" height="14" rx="2"/><rect x="18" y="5" width="4" height="11" rx="2"/></svg>"#
                    .to_owned()
            } else {
                format!(r#"<b class="agent-status {status}" aria-label="{status}"></b>"#)
            };
            format!(
                r#"<div class="agent-row"><span>{} <i>·</i> {}</span>{indicator}</div>"#,
                escape(workspace),
                escape(pane)
            )
        })
        .collect::<String>();
    let idle_logo = if rows.is_empty() && title == "Agents idle" {
        r#"<div class="agent-idle"><img src="/components/agents/pi-logo.svg" alt="Pi"></div>"#
    } else {
        ""
    };
    let action = if !blocked.is_empty() || completed {
        r#" onclick="fetch('/components/agents/focus',{method:'POST'})" role="button""#
    } else {
        r#" onclick="openAgents()" role="button""#
    };
    let all_agents = state
        .agents
        .iter()
        .map(|agent| {
            let status = escape(&agent.status);
            format!(
                r#"<button class="agent-detail {status}" onclick="focusAgent('{}')"><span><strong>{}</strong><small>· {}</small></span><b>{status}</b></button>"#,
                escape(&agent.pane_id),
                escape(&agent.workspace),
                escape(&agent.pane)
            )
        })
        .collect::<String>();
    format!(
        r#"<section id="agent-card" class="{class}"{action} hx-swap-oob="outerHTML"><div class="agent-list">{rows}{idle_logo}</div></section><div id="agent-dialog-content" class="agent-dialog-content" hx-swap-oob="outerHTML"><aside><img src="/components/agents/pi-logo.svg" alt="Pi"></aside><section>{all_agents}</section></div>"#
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

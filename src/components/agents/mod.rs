use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "windows", test))]
use std::collections::{HashMap, VecDeque};

#[cfg(target_os = "windows")]
use axum::{Json, extract::DefaultBodyLimit, http::HeaderMap, routing::post};
use axum::{
    Router,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
#[cfg(target_os = "linux")]
use axum::{extract::Path, routing::post};
#[cfg(any(target_os = "windows", test))]
use serde::Deserialize;

use crate::components::GridSize;

#[cfg(target_os = "linux")]
use self::backend::HerdrBackend;
use super::dock_component;

#[cfg(target_os = "linux")]
pub(crate) mod backend;

dock_component!(Agents, "agents", GridSize::ONE_BY_ONE, routes());

const DONE_STATE_TTL: Duration = Duration::from_secs(10);
#[cfg(any(target_os = "windows", test))]
const MAX_CODEX_SESSIONS: usize = 128;
#[cfg(any(target_os = "windows", test))]
const MAX_RECENT_CODEX_TURNS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentState {
    Idle,
    Working,
    Blocked,
    Done,
}

impl AgentState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    #[cfg(target_os = "linux")]
    fn from_herdr(value: &str) -> Self {
        match value {
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            _ => Self::Idle,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentStatus {
    pub workspace: String,
    pub pane: String,
    pub pane_id: String,
    pub status: AgentState,
}

#[derive(Clone, Default)]
struct AgentWidgetState {
    agents: Vec<AgentStatus>,
    completed: Vec<AgentStatus>,
    completed_at: Option<Instant>,
    #[cfg(any(target_os = "windows", test))]
    codex_done_at: HashMap<String, Instant>,
    #[cfg(any(target_os = "windows", test))]
    codex_lifecycle: HashMap<String, CodexLifecycle>,
    #[cfg(any(target_os = "windows", test))]
    ended_codex_sessions: VecDeque<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Default)]
struct CodexLifecycle {
    current_turn_id: Option<String>,
    completed_turn_ids: VecDeque<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "hook_event_name")]
enum CodexHookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PermissionRequest,
    Stop,
    SessionEnd,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Deserialize)]
struct CodexHookPayload {
    session_id: String,
    cwd: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(flatten)]
    event: CodexHookEvent,
}

#[cfg(any(target_os = "windows", test))]
impl CodexHookPayload {
    fn is_valid(&self) -> bool {
        let session_id = self.session_id.trim();
        let cwd = self.cwd.trim();
        let turn_is_valid = if matches!(
            self.event,
            CodexHookEvent::SessionStart | CodexHookEvent::SessionEnd
        ) {
            true
        } else {
            self.turn_id
                .as_deref()
                .is_some_and(|turn_id| !turn_id.trim().is_empty() && turn_id.len() <= 512)
        };
        !session_id.is_empty()
            && session_id.len() <= 512
            && !cwd.is_empty()
            && cwd.len() <= 4_096
            && self.model.as_deref().is_none_or(|model| model.len() <= 512)
            && self
                .source
                .as_deref()
                .is_none_or(|source| source.len() <= 64)
            && turn_is_valid
    }
}

type Invalidate = Arc<dyn Fn() + Send + Sync>;
static STATE: OnceLock<Mutex<AgentWidgetState>> = OnceLock::new();
static INVALIDATE: OnceLock<Invalidate> = OnceLock::new();

fn routes() -> Router {
    let router = Router::new()
        .route("/component.js", get(component_javascript))
        .route("/pi-logo.svg", get(pi_logo));
    #[cfg(target_os = "linux")]
    let router = router
        .route("/focus", post(focus_agent))
        .route("/focus/{pane_id}", post(focus_agent_by_id));
    #[cfg(target_os = "windows")]
    let router = router.route(
        "/hooks/codex",
        post(codex_hook).layer(DefaultBodyLimit::max(
            crate::codex_hooks::MAX_HOOK_PAYLOAD_BYTES as usize,
        )),
    );
    router
}

async fn pi_logo() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        include_str!("../../../assets/web/pi-logo.svg"),
    )
}

async fn component_javascript() -> impl IntoResponse {
    #[cfg(target_os = "linux")]
    let source = include_str!("component.js");
    #[cfg(target_os = "windows")]
    let source = include_str!("component.windows.js");
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        source,
    )
}

fn state() -> &'static Mutex<AgentWidgetState> {
    STATE.get_or_init(|| Mutex::new(AgentWidgetState::default()))
}

fn snapshot() -> AgentWidgetState {
    state()
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
            #[cfg(target_os = "linux")]
            let mut previous = Vec::<AgentStatus>::new();
            while !shutdown.load(Ordering::Relaxed) {
                #[cfg(target_os = "linux")]
                refresh_herdr(&mut previous);

                let changed = {
                    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
                    expire_transient_state(&mut state, Instant::now())
                };
                if changed {
                    invalidate();
                }
                thread::sleep(Duration::from_millis(500));
            }
        })
        .expect("failed to start agents observer")
}

#[cfg(target_os = "linux")]
fn refresh_herdr(previous: &mut Vec<AgentStatus>) {
    let Ok(current) = HerdrBackend.agents() else {
        return;
    };
    if current == *previous {
        return;
    }
    let completed = current
        .iter()
        .filter(|agent| {
            agent.status == AgentState::Done
                || (agent.status == AgentState::Idle
                    && previous.iter().any(|old| {
                        old.workspace == agent.workspace
                            && old.pane == agent.pane
                            && old.status == AgentState::Working
                    }))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
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
    previous.clone_from(&current);
    drop(state);
    invalidate();
}

fn expire_transient_state(state: &mut AgentWidgetState, now: Instant) -> bool {
    let mut changed = false;
    if state
        .completed_at
        .is_some_and(|at| now.saturating_duration_since(at) >= DONE_STATE_TTL)
    {
        state.completed.clear();
        state.completed_at = None;
        changed = true;
    }

    #[cfg(any(target_os = "windows", test))]
    {
        let expired = state
            .codex_done_at
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) >= DONE_STATE_TTL)
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        for session_id in expired {
            state.codex_done_at.remove(&session_id);
            if let Some(agent) = state
                .agents
                .iter_mut()
                .find(|agent| agent.pane_id == session_id && agent.status == AgentState::Done)
            {
                agent.status = AgentState::Idle;
                changed = true;
            }
        }
    }
    changed
}

#[cfg(target_os = "windows")]
async fn codex_hook(headers: HeaderMap, Json(payload): Json<CodexHookPayload>) -> StatusCode {
    let Some(token) = headers
        .get(crate::codex_hooks::CODEX_HOOK_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::UNAUTHORIZED;
    };
    match crate::codex_hooks::validate_auth_token(token) {
        Ok(true) => {}
        Ok(false) => return StatusCode::UNAUTHORIZED,
        Err(error) => {
            tracing::warn!(%error, "failed to validate Codex hook token");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    if !payload.is_valid() {
        return StatusCode::BAD_REQUEST;
    }

    let changed = {
        let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
        apply_codex_hook(&mut state, payload, Instant::now())
    };
    if changed {
        invalidate();
    }
    StatusCode::NO_CONTENT
}

#[cfg(any(target_os = "windows", test))]
fn apply_codex_hook(state: &mut AgentWidgetState, payload: CodexHookPayload, now: Instant) -> bool {
    let session_id = payload.session_id.clone();
    if payload.event == CodexHookEvent::SessionEnd {
        let old_len = state.agents.len();
        state.agents.retain(|agent| agent.pane_id != session_id);
        state.codex_done_at.remove(&session_id);
        state.codex_lifecycle.remove(&session_id);
        state.completed.retain(|agent| agent.pane_id != session_id);
        remember_ended_session(state, session_id);
        return state.agents.len() != old_len;
    }

    if payload.event == CodexHookEvent::SessionStart {
        let was_ended = state
            .ended_codex_sessions
            .iter()
            .any(|ended| ended == &session_id);
        if was_ended && payload.source.as_deref() != Some("resume") {
            return false;
        }
        state
            .ended_codex_sessions
            .retain(|ended| ended != &session_id);
        state.codex_lifecycle.entry(session_id.clone()).or_default();
        return upsert_codex_agent(state, payload, AgentState::Idle, false);
    }

    if state
        .ended_codex_sessions
        .iter()
        .any(|ended| ended == &session_id)
    {
        return false;
    }
    let Some(turn_id) = payload.turn_id.as_deref() else {
        return false;
    };
    let lifecycle = state.codex_lifecycle.entry(session_id.clone()).or_default();
    if !accept_codex_turn_event(lifecycle, payload.event, turn_id) {
        return false;
    }

    let status = match payload.event {
        CodexHookEvent::UserPromptSubmit
        | CodexHookEvent::PreToolUse
        | CodexHookEvent::PostToolUse => AgentState::Working,
        CodexHookEvent::PermissionRequest => AgentState::Blocked,
        CodexHookEvent::Stop => AgentState::Done,
        CodexHookEvent::SessionStart | CodexHookEvent::SessionEnd => {
            unreachable!("session lifecycle events were handled above")
        }
    };
    let changed = upsert_codex_agent(state, payload, status, true);
    if status == AgentState::Done {
        state.codex_done_at.insert(session_id, now);
    } else {
        state.codex_done_at.remove(&session_id);
    }
    changed
}

#[cfg(any(target_os = "windows", test))]
fn upsert_codex_agent(
    state: &mut AgentWidgetState,
    payload: CodexHookPayload,
    status: AgentState,
    update_existing_status: bool,
) -> bool {
    let workspace = workspace_name(&payload.cwd);
    let pane = payload
        .model
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| "Codex Desktop".to_owned());
    if let Some(agent) = state
        .agents
        .iter_mut()
        .find(|agent| agent.pane_id == payload.session_id)
    {
        let next_status = if update_existing_status {
            status
        } else {
            agent.status
        };
        let changed =
            agent.workspace != workspace || agent.pane != pane || agent.status != next_status;
        agent.workspace = workspace;
        agent.pane = pane;
        agent.status = next_status;
        return changed;
    }

    if state.agents.len() >= MAX_CODEX_SESSIONS {
        let evicted = state.agents.remove(0);
        state.codex_done_at.remove(&evicted.pane_id);
        state.codex_lifecycle.remove(&evicted.pane_id);
    }
    state.agents.push(AgentStatus {
        workspace,
        pane,
        pane_id: payload.session_id,
        status,
    });
    true
}

#[cfg(any(target_os = "windows", test))]
fn accept_codex_turn_event(
    lifecycle: &mut CodexLifecycle,
    event: CodexHookEvent,
    turn_id: &str,
) -> bool {
    if lifecycle
        .completed_turn_ids
        .iter()
        .any(|completed| completed == turn_id)
    {
        return false;
    }
    match event {
        CodexHookEvent::UserPromptSubmit => {
            lifecycle.current_turn_id = Some(turn_id.to_owned());
            true
        }
        CodexHookEvent::Stop => {
            let applies = lifecycle
                .current_turn_id
                .as_deref()
                .is_none_or(|current| current == turn_id);
            lifecycle.current_turn_id = lifecycle
                .current_turn_id
                .take()
                .filter(|current| !applies || current != turn_id);
            remember_completed_turn(lifecycle, turn_id);
            applies
        }
        CodexHookEvent::PreToolUse
        | CodexHookEvent::PostToolUse
        | CodexHookEvent::PermissionRequest => match lifecycle.current_turn_id.as_deref() {
            Some(current) => current == turn_id,
            None => {
                lifecycle.current_turn_id = Some(turn_id.to_owned());
                true
            }
        },
        CodexHookEvent::SessionStart | CodexHookEvent::SessionEnd => false,
    }
}

#[cfg(any(target_os = "windows", test))]
fn remember_completed_turn(lifecycle: &mut CodexLifecycle, turn_id: &str) {
    if lifecycle.completed_turn_ids.len() >= MAX_RECENT_CODEX_TURNS {
        lifecycle.completed_turn_ids.pop_front();
    }
    lifecycle.completed_turn_ids.push_back(turn_id.to_owned());
}

#[cfg(any(target_os = "windows", test))]
fn remember_ended_session(state: &mut AgentWidgetState, session_id: String) {
    state
        .ended_codex_sessions
        .retain(|ended| ended != &session_id);
    if state.ended_codex_sessions.len() >= MAX_CODEX_SESSIONS {
        state.ended_codex_sessions.pop_front();
    }
    state.ended_codex_sessions.push_back(session_id);
}

#[cfg(any(target_os = "windows", test))]
fn workspace_name(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

#[cfg(target_os = "linux")]
async fn focus_agent_by_id(Path(pane_id): Path<String>) -> StatusCode {
    let focus_id = pane_id.clone();
    match tokio::task::spawn_blocking(move || HerdrBackend.focus(&focus_id)).await {
        Ok(Ok(())) => {
            let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
            state.completed.retain(|agent| agent.pane_id != pane_id);
            if let Some(agent) = state
                .agents
                .iter_mut()
                .find(|agent| agent.pane_id == pane_id && agent.status == AgentState::Done)
            {
                agent.status = AgentState::Idle;
            }
            if state.completed.is_empty()
                && !state
                    .agents
                    .iter()
                    .any(|agent| agent.status == AgentState::Done)
            {
                state.completed_at = None;
            }
            drop(state);
            invalidate();
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(target_os = "linux")]
async fn focus_agent() -> StatusCode {
    let pane_id = {
        let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
        let pane_id = state
            .agents
            .iter()
            .find(|agent| agent.status == AgentState::Blocked)
            .or_else(|| {
                state
                    .agents
                    .iter()
                    .find(|agent| agent.status == AgentState::Done)
            })
            .or_else(|| state.completed.first())
            .map(|agent| agent.pane_id.clone());
        if let Some(pane_id) = &pane_id {
            state.completed.retain(|agent| &agent.pane_id != pane_id);
            if let Some(agent) = state
                .agents
                .iter_mut()
                .find(|agent| &agent.pane_id == pane_id)
            {
                agent.status = AgentState::Idle;
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
    render_fragment(&snapshot())
}

fn render_fragment(state: &AgentWidgetState) -> String {
    let blocked = state
        .agents
        .iter()
        .filter(|agent| agent.status == AgentState::Blocked)
        .collect::<Vec<_>>();
    let working = state
        .agents
        .iter()
        .filter(|agent| agent.status == AgentState::Working)
        .collect::<Vec<_>>();
    let mut completed_agents = state.completed.clone();
    for agent in state
        .agents
        .iter()
        .filter(|agent| agent.status == AgentState::Done)
    {
        if !completed_agents
            .iter()
            .any(|item| item.pane_id == agent.pane_id)
        {
            completed_agents.push(agent.clone());
        }
    }
    let completed = !completed_agents.is_empty()
        && (state
            .agents
            .iter()
            .any(|agent| agent.status == AgentState::Done)
            || state
                .completed_at
                .is_some_and(|at| at.elapsed() < DONE_STATE_TTL));
    let (class, title, agents): (&str, &str, Vec<(&str, &str, AgentState)>) = if !blocked.is_empty()
    {
        (
            "card agent-card attention",
            "Attention needed",
            blocked
                .iter()
                .map(|agent| {
                    (
                        agent.workspace.as_str(),
                        agent.pane.as_str(),
                        AgentState::Blocked,
                    )
                })
                .collect(),
        )
    } else if completed {
        (
            "card agent-card completed",
            "Agent completed",
            completed_agents
                .iter()
                .map(|agent| {
                    (
                        agent.workspace.as_str(),
                        agent.pane.as_str(),
                        AgentState::Done,
                    )
                })
                .collect(),
        )
    } else if !working.is_empty() {
        (
            "card agent-card running",
            "Agents running",
            working
                .iter()
                .map(|agent| {
                    (
                        agent.workspace.as_str(),
                        agent.pane.as_str(),
                        AgentState::Working,
                    )
                })
                .collect(),
        )
    } else {
        ("card agent-card idle", "Agents idle", Vec::new())
    };
    let rows = agents
        .into_iter()
        .map(|(workspace, pane, status)| {
            let status = status.as_str();
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
    let action = card_action(!blocked.is_empty() || completed);
    let all_agents = state
        .agents
        .iter()
        .map(render_agent_detail)
        .collect::<String>();
    format!(
        r#"<section id="agent-card" class="{class}"{action} hx-swap-oob="outerHTML"><div class="agent-list">{rows}{idle_logo}</div></section><div id="agent-dialog-content" class="agent-dialog-content" hx-swap-oob="outerHTML"><aside><img src="/components/agents/pi-logo.svg" alt="Pi"></aside><section>{all_agents}</section></div>"#
    )
}

#[cfg(target_os = "linux")]
fn card_action(needs_focus: bool) -> &'static str {
    if needs_focus {
        r#" onclick="fetch('/components/agents/focus',{method:'POST'})" role="button""#
    } else {
        r#" onclick="openAgents()" role="button""#
    }
}

#[cfg(target_os = "windows")]
fn card_action(_needs_focus: bool) -> &'static str {
    r#" onclick="openAgents()" role="button""#
}

#[cfg(target_os = "linux")]
fn render_agent_detail(agent: &AgentStatus) -> String {
    let status = agent.status.as_str();
    format!(
        r#"<button class="agent-detail {status}" onclick="focusAgent('{}')"><span><strong>{}</strong><small>· {}</small></span><b>{status}</b></button>"#,
        escape(&agent.pane_id),
        escape(&agent.workspace),
        escape(&agent.pane)
    )
}

#[cfg(target_os = "windows")]
fn render_agent_detail(agent: &AgentStatus) -> String {
    let status = agent.status.as_str();
    format!(
        r#"<div class="agent-detail {status}"><span><strong>{}</strong><small>· {}</small></span><b>{status}</b></div>"#,
        escape(&agent.workspace),
        escape(&agent.pane)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(event: CodexHookEvent, session_id: &str) -> CodexHookPayload {
        let session_event = matches!(
            event,
            CodexHookEvent::SessionStart | CodexHookEvent::SessionEnd
        );
        CodexHookPayload {
            session_id: session_id.to_owned(),
            cwd: r#"C:\Users\Ada\Projects\raylander"#.to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            source: (event == CodexHookEvent::SessionStart).then(|| "startup".to_owned()),
            turn_id: (!session_event).then(|| "turn-1".to_owned()),
            event,
        }
    }

    #[test]
    fn deserializes_only_supported_typed_codex_events() {
        let payload: CodexHookPayload = serde_json::from_str(
            r#"{"session_id":"thread-1","turn_id":"turn-1","cwd":"C:\\repo","model":"codex","hook_event_name":"PermissionRequest","tool_name":"Shell"}"#,
        )
        .unwrap();
        assert_eq!(payload.event, CodexHookEvent::PermissionRequest);
        assert!(payload.is_valid());
        assert!(
            serde_json::from_str::<CodexHookPayload>(
                r#"{"session_id":"thread-1","cwd":"C:\\repo","hook_event_name":"Unknown"}"#
            )
            .is_err()
        );
        assert!(
            !CodexHookPayload {
                session_id: String::new(),
                cwd: r#"C:\repo"#.to_owned(),
                model: None,
                source: Some("startup".to_owned()),
                turn_id: None,
                event: CodexHookEvent::SessionStart,
            }
            .is_valid()
        );
    }

    #[test]
    fn windows_component_script_has_no_conversation_focus_action() {
        let source = include_str!("component.windows.js");
        assert!(source.contains("openAgents"));
        assert!(!source.contains("focus"));
        assert!(!source.contains("/components/agents/focus"));
    }

    #[test]
    fn codex_events_drive_the_full_session_lifecycle() {
        let mut state = AgentWidgetState::default();
        let now = Instant::now();
        for (event, expected) in [
            (CodexHookEvent::SessionStart, AgentState::Idle),
            (CodexHookEvent::UserPromptSubmit, AgentState::Working),
            (CodexHookEvent::PreToolUse, AgentState::Working),
            (CodexHookEvent::PostToolUse, AgentState::Working),
            (CodexHookEvent::PermissionRequest, AgentState::Blocked),
            (CodexHookEvent::Stop, AgentState::Done),
        ] {
            apply_codex_hook(&mut state, payload(event, "thread-1"), now);
            assert_eq!(state.agents.len(), 1);
            assert_eq!(state.agents[0].status, expected);
            assert_eq!(state.agents[0].workspace, "raylander");
        }
        assert!(apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::SessionEnd, "thread-1"),
            now
        ));
        assert!(state.agents.is_empty());
        assert!(state.codex_done_at.is_empty());
    }

    #[test]
    fn activity_clears_blocked_and_done_expiry_returns_to_idle() {
        let mut state = AgentWidgetState::default();
        let now = Instant::now();
        apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::PermissionRequest, "thread-1"),
            now,
        );
        apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::PreToolUse, "thread-1"),
            now,
        );
        assert_eq!(state.agents[0].status, AgentState::Working);
        apply_codex_hook(&mut state, payload(CodexHookEvent::Stop, "thread-1"), now);
        assert!(!expire_transient_state(
            &mut state,
            now + DONE_STATE_TTL - Duration::from_millis(1)
        ));
        assert!(expire_transient_state(&mut state, now + DONE_STATE_TTL));
        assert_eq!(state.agents[0].status, AgentState::Idle);
    }

    #[test]
    fn late_async_events_do_not_regress_completed_or_ended_sessions() {
        let mut state = AgentWidgetState::default();
        let now = Instant::now();
        apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::UserPromptSubmit, "thread-1"),
            now,
        );
        apply_codex_hook(&mut state, payload(CodexHookEvent::Stop, "thread-1"), now);
        assert!(!apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::PostToolUse, "thread-1"),
            now
        ));
        assert_eq!(state.agents[0].status, AgentState::Done);

        let mut next_turn = payload(CodexHookEvent::UserPromptSubmit, "thread-1");
        next_turn.turn_id = Some("turn-2".to_owned());
        apply_codex_hook(&mut state, next_turn, now);
        let late_stop = payload(CodexHookEvent::Stop, "thread-1");
        assert!(!apply_codex_hook(&mut state, late_stop, now));
        assert_eq!(state.agents[0].status, AgentState::Working);

        apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::SessionEnd, "thread-1"),
            now,
        );
        let mut late_tool = payload(CodexHookEvent::PreToolUse, "thread-1");
        late_tool.turn_id = Some("turn-2".to_owned());
        assert!(!apply_codex_hook(&mut state, late_tool, now));
        assert!(state.agents.is_empty());

        let mut resumed = payload(CodexHookEvent::SessionStart, "thread-1");
        resumed.source = Some("resume".to_owned());
        assert!(apply_codex_hook(&mut state, resumed, now));
        assert_eq!(state.agents[0].status, AgentState::Idle);
    }

    #[test]
    fn codex_sessions_are_bounded_and_updates_do_not_duplicate() {
        let mut state = AgentWidgetState::default();
        let now = Instant::now();
        for index in 0..=MAX_CODEX_SESSIONS {
            apply_codex_hook(
                &mut state,
                payload(CodexHookEvent::SessionStart, &format!("thread-{index}")),
                now,
            );
        }
        assert_eq!(state.agents.len(), MAX_CODEX_SESSIONS);
        assert!(!state.agents.iter().any(|agent| agent.pane_id == "thread-0"));
        apply_codex_hook(
            &mut state,
            payload(CodexHookEvent::PreToolUse, "thread-1"),
            now,
        );
        assert_eq!(
            state
                .agents
                .iter()
                .filter(|agent| agent.pane_id == "thread-1")
                .count(),
            1
        );
    }

    #[test]
    fn rendering_escapes_hook_metadata() {
        let state = AgentWidgetState {
            agents: vec![AgentStatus {
                workspace: "<repo>".to_owned(),
                pane: "model\"'".to_owned(),
                pane_id: "thread-1".to_owned(),
                status: AgentState::Blocked,
            }],
            ..AgentWidgetState::default()
        };
        let html = render_fragment(&state);
        assert!(html.contains("&lt;repo&gt;"));
        assert!(html.contains("model&quot;&#39;"));
        assert!(!html.contains("<repo>"));
    }
}

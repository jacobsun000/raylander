//! Codex Desktop hook integration.
//!
//! The high-level [`install`], [`status`], and [`uninstall`] functions are
//! intentionally supported only on Windows. They merge Raylander's handlers
//! into `CODEX_HOME/hooks.json` (or `USERPROFILE/.codex/hooks.json`) without
//! replacing hooks owned by the user or another integration.
//!
//! Application integration has two small entry points:
//! - Route an invocation whose first argument is [`CODEX_HOOK_NOTIFY_ARGUMENT`]
//!   to [`forward_stdin_to_daemon`]. Codex supplies the hook JSON on stdin.
//! - On the daemon route at [`DAEMON_NOTIFICATION_PATH`], obtain the
//!   [`CODEX_HOOK_TOKEN_HEADER`] header and call [`validate_auth_token`] before
//!   accepting the body.
//!
//! [`ensure_auth_token`] is called before handlers are installed so an
//! installed handler never intentionally targets a daemon without a token.

use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Map, Value, json};

/// Argument used by installed hooks to select the hook-notification entry point.
pub const CODEX_HOOK_NOTIFY_ARGUMENT: &str = "codex-hook";
/// Header carrying the per-user Raylander/Codex authentication token.
pub const CODEX_HOOK_TOKEN_HEADER: &str = "x-raylander-codex-hook-token";
/// Suggested loopback daemon route for hook notifications.
pub const DAEMON_NOTIFICATION_PATH: &str = "/components/agents/hooks/codex";
/// Codex events observed by Raylander.
pub const HOOK_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];

const KNOWN_HOOK_EVENTS: [&str; 11] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
const OWNER_TAG: &str = "Raylander Codex Desktop hook";
pub(crate) const MAX_HOOK_PAYLOAD_BYTES: u64 = 64 * 1024;
const SESSION_END_TIMEOUT_SECONDS: u64 = 3;
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LENGTH: usize = TOKEN_BYTES * 2;
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Overall state of Raylander's Codex hook installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookState {
    /// Hook installation is deliberately unavailable on this platform.
    Unsupported,
    /// No Raylander-owned handlers were found.
    NotInstalled,
    /// Some expected handlers are absent.
    Partial,
    /// Owned handlers exist but differ from the current executable/configuration.
    Stale,
    /// All expected handlers match the current executable/configuration.
    Installed,
    /// `hooks.json` could not safely be interpreted and was not changed.
    Malformed,
}

/// Detailed result returned by installation/status/removal operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookStatus {
    pub state: HookState,
    pub hooks_path: Option<PathBuf>,
    /// Events with exactly one current Raylander handler.
    pub installed_events: Vec<String>,
    /// Events with no Raylander handler.
    pub missing_events: Vec<String>,
    /// Events with an outdated or duplicate Raylander handler.
    pub stale_events: Vec<String>,
    /// Parse or shape error when [`HookState::Malformed`] is reported.
    pub detail: Option<String>,
}

impl HookStatus {
    #[cfg(not(windows))]
    fn unsupported() -> Self {
        Self {
            state: HookState::Unsupported,
            hooks_path: codex_home().ok().map(|home| home.join("hooks.json")),
            installed_events: Vec::new(),
            missing_events: Vec::new(),
            stale_events: Vec::new(),
            detail: None,
        }
    }

    fn not_installed(path: PathBuf) -> Self {
        Self {
            state: HookState::NotInstalled,
            hooks_path: Some(path),
            installed_events: Vec::new(),
            missing_events: HOOK_EVENTS
                .iter()
                .map(|event| (*event).to_owned())
                .collect(),
            stale_events: Vec::new(),
            detail: None,
        }
    }

    fn malformed(path: PathBuf, error: impl std::fmt::Display) -> Self {
        Self {
            state: HookState::Malformed,
            hooks_path: Some(path),
            installed_events: Vec::new(),
            missing_events: Vec::new(),
            stale_events: Vec::new(),
            detail: Some(error.to_string()),
        }
    }
}

/// Locate Codex's home directory.
///
/// `CODEX_HOME` takes precedence. Windows then uses `USERPROFILE/.codex`.
/// The `HOME/.codex` fallback allows path/status tooling to remain useful on
/// non-Windows hosts without enabling production hook installation there.
pub fn codex_home() -> Result<PathBuf> {
    if let Some(path) = nonempty_env_path("CODEX_HOME") {
        return Ok(path);
    }
    if let Some(profile) = nonempty_env_path("USERPROFILE") {
        return Ok(profile.join(".codex"));
    }
    #[cfg(not(windows))]
    if let Some(home) = nonempty_env_path("HOME") {
        return Ok(home.join(".codex"));
    }
    bail!("neither CODEX_HOME nor USERPROFILE is set")
}

/// Return the platform-specific persisted authentication-token path.
pub fn auth_token_path() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let root = crate::config::roaming_app_data_root()?;
        Ok(root.join("Raylander").join("codex-hooks.token"))
    }

    #[cfg(not(windows))]
    {
        let root = nonempty_env_path("XDG_DATA_HOME")
            .or_else(|| nonempty_env_path("HOME").map(|home| home.join(".local/share")))
            .context("XDG_DATA_HOME and HOME are both unset")?;
        Ok(root.join("raylander").join("codex-hooks.token"))
    }
}

/// Install or refresh all Raylander Codex hooks on Windows.
///
/// Calling this repeatedly is idempotent. If Raylander moved, owned handlers
/// are rewritten with the new absolute executable path. On non-Windows hosts
/// this is a no-op returning [`HookState::Unsupported`].
#[cfg(windows)]
pub fn install() -> Result<HookStatus> {
    let home = codex_home()?;
    let executable = absolute_hook_executable()?;
    ensure_auth_token()?;
    install_at(&home, &executable)
}

/// Non-Windows no-op counterpart to [`install`].
#[cfg(not(windows))]
pub fn install() -> Result<HookStatus> {
    Ok(HookStatus::unsupported())
}

/// Inspect the current Raylander Codex hook installation on Windows.
#[cfg(windows)]
pub fn status() -> Result<HookStatus> {
    status_at(&codex_home()?, &absolute_hook_executable()?)
}

/// Non-Windows status counterpart reporting [`HookState::Unsupported`].
#[cfg(not(windows))]
pub fn status() -> Result<HookStatus> {
    Ok(HookStatus::unsupported())
}

/// Remove only Raylander-owned Codex handlers on Windows.
#[cfg(windows)]
pub fn uninstall() -> Result<HookStatus> {
    uninstall_at(&codex_home()?, &absolute_hook_executable()?)
}

/// Non-Windows no-op counterpart to [`uninstall`].
#[cfg(not(windows))]
pub fn uninstall() -> Result<HookStatus> {
    Ok(HookStatus::unsupported())
}

/// Install hooks at an explicitly supplied Codex home.
///
/// This platform-neutral lower-level API is useful to installers and tests.
/// `executable` must be absolute. It performs the same atomic merge as the
/// Windows high-level API but does not create an authentication token.
pub fn install_at(codex_home: &Path, executable: &Path) -> Result<HookStatus> {
    ensure!(
        executable.is_absolute(),
        "Codex hook executable path must be absolute"
    );
    let current = status_at(codex_home, executable)?;
    if current.state == HookState::Installed {
        return Ok(current);
    }
    let hooks_path = codex_home.join("hooks.json");
    let original = read_document_for_update(&hooks_path)?;
    let mut document = original.clone().unwrap_or_else(|| json!({ "hooks": {} }));
    let expected_command = windows_hook_command(executable)?;

    {
        let hooks = hooks_object_mut(&mut document)?;
        for event in HOOK_EVENTS {
            if let Some(groups) = hooks.get_mut(event) {
                remove_owned_handlers(groups, event)?;
            }
        }
        for event in HOOK_EVENTS {
            let groups = hooks
                .entry(event.to_owned())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| anyhow!("hooks.{event} must be an array"))?;
            groups.push(owned_matcher_group(event, &expected_command));
        }
    }

    if original
        .as_ref()
        .is_none_or(|original_document| original_document != &document)
    {
        write_json_atomically(&hooks_path, &document)
            .with_context(|| format!("failed to update {}", hooks_path.display()))?;
    }
    status_at(codex_home, executable)
}

/// Inspect hooks at an explicitly supplied Codex home.
pub fn status_at(codex_home: &Path, executable: &Path) -> Result<HookStatus> {
    ensure!(
        executable.is_absolute(),
        "Codex hook executable path must be absolute"
    );
    let path = codex_home.join("hooks.json");
    let source = match fs::read(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(HookStatus::not_installed(path));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let document: Value = match serde_json::from_slice(&source) {
        Ok(document) => document,
        Err(error) => return Ok(HookStatus::malformed(path, error)),
    };
    let expected_command = windows_hook_command(executable)?;
    let hooks = match hooks_object(&document).and_then(|hooks| {
        validate_hooks_shape(hooks)?;
        Ok(hooks)
    }) {
        Ok(hooks) => hooks,
        Err(error) => return Ok(HookStatus::malformed(path, error)),
    };

    let mut installed_events = Vec::new();
    let mut missing_events = Vec::new();
    let mut stale_events = Vec::new();
    for event in HOOK_EVENTS {
        let Some(groups) = hooks.get(event) else {
            missing_events.push(event.to_owned());
            continue;
        };
        let (owned_count, current_count) = match inspect_event(groups, event, &expected_command) {
            Ok(counts) => counts,
            Err(error) => return Ok(HookStatus::malformed(path, error)),
        };
        match (owned_count, current_count) {
            (0, _) => missing_events.push(event.to_owned()),
            (1, 1) => installed_events.push(event.to_owned()),
            _ => stale_events.push(event.to_owned()),
        }
    }

    let state = if !stale_events.is_empty() {
        HookState::Stale
    } else if !missing_events.is_empty() {
        if installed_events.is_empty() {
            HookState::NotInstalled
        } else {
            HookState::Partial
        }
    } else {
        HookState::Installed
    };
    Ok(HookStatus {
        state,
        hooks_path: Some(path),
        installed_events,
        missing_events,
        stale_events,
        detail: None,
    })
}

/// Remove hooks at an explicitly supplied Codex home.
///
/// Only command handlers carrying Raylander's `statusMessage` ownership tag
/// are removed. Other handlers, matcher groups, event state, and top-level
/// fields are retained.
pub fn uninstall_at(codex_home: &Path, executable: &Path) -> Result<HookStatus> {
    ensure!(
        executable.is_absolute(),
        "Codex hook executable path must be absolute"
    );
    let path = codex_home.join("hooks.json");
    let Some(mut document) = read_document_for_update(&path)? else {
        return Ok(HookStatus::not_installed(path));
    };
    let original = document.clone();
    {
        let hooks = hooks_object_mut(&mut document)?;
        for event in HOOK_EVENTS {
            let remove_empty_event = if let Some(groups) = hooks.get_mut(event) {
                let removed = remove_owned_handlers(groups, event)?;
                removed && groups.as_array().is_some_and(std::vec::Vec::is_empty)
            } else {
                false
            };
            if remove_empty_event {
                hooks.remove(event);
            }
        }
    }
    if document != original {
        write_json_atomically(&path, &document)
            .with_context(|| format!("failed to update {}", path.display()))?;
    }
    status_at(codex_home, executable)
}

/// Ensure that a random, persistent per-user authentication token exists.
///
/// Existing malformed token files are rejected rather than silently replaced.
/// On Unix, the containing directory and token are tightened to modes 0700 and
/// 0600 respectively. Windows relies on the per-user roaming AppData ACL.
pub fn ensure_auth_token() -> Result<String> {
    ensure_auth_token_at(&auth_token_path()?)
}

/// Validate a candidate against the persisted authentication token.
///
/// A missing token returns `false`; malformed or unreadable token files return
/// an error. Comparison is constant-time for the fixed-size generated tokens.
pub fn validate_auth_token(candidate: &str) -> Result<bool> {
    let Some(expected) = read_auth_token_at(&auth_token_path()?)? else {
        return Ok(false);
    };
    Ok(constant_time_token_eq(
        expected.as_bytes(),
        candidate.as_bytes(),
    ))
}

/// Read one Codex hook payload from stdin and POST it to the loopback daemon.
pub fn forward_stdin_to_daemon(address: SocketAddr) -> Result<()> {
    let stdin = io::stdin();
    forward_reader_to_daemon(stdin.lock(), address)
}

/// Read one Codex hook payload and POST it to the loopback daemon.
///
/// This is the testable/read-stream counterpart to [`forward_stdin_to_daemon`].
pub fn forward_reader_to_daemon(reader: impl Read, address: SocketAddr) -> Result<()> {
    let mut payload = Vec::new();
    reader
        .take(MAX_HOOK_PAYLOAD_BYTES + 1)
        .read_to_end(&mut payload)
        .context("failed to read Codex hook payload from stdin")?;
    ensure!(
        payload.len() as u64 <= MAX_HOOK_PAYLOAD_BYTES,
        "Codex hook payload exceeds {MAX_HOOK_PAYLOAD_BYTES} bytes"
    );
    post_payload_to_daemon(address, &payload)
}

/// POST an already-read hook payload to the loopback daemon with authentication.
pub fn post_payload_to_daemon(address: SocketAddr, payload: &[u8]) -> Result<()> {
    let token = ensure_auth_token()?;
    post_payload_with_token(address, payload, &token)
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn absolute_hook_executable() -> Result<PathBuf> {
    let executable = env::current_exe().context("failed to locate the Raylander executable")?;
    ensure!(
        executable.is_absolute(),
        "current Raylander executable path is not absolute"
    );
    if executable
        .file_stem()
        .is_some_and(|name| name.eq_ignore_ascii_case("raylander-tray"))
    {
        let console = executable.with_file_name("raylander.exe");
        ensure!(
            console.is_file(),
            "{} is missing; keep raylander.exe beside raylander-tray.exe",
            console.display()
        );
        Ok(console)
    } else {
        Ok(executable)
    }
}

fn windows_hook_command(executable: &Path) -> Result<String> {
    let executable = executable
        .to_str()
        .context("Raylander executable path is not valid Unicode")?;
    ensure!(
        !executable.contains('"'),
        "Raylander executable path contains an unsupported quote"
    );
    Ok(format!("\"{executable}\" {CODEX_HOOK_NOTIFY_ARGUMENT}"))
}

fn owned_matcher_group(event: &str, command: &str) -> Value {
    let mut handler = Map::from_iter([
        ("type".to_owned(), Value::String("command".to_owned())),
        ("command".to_owned(), Value::String(command.to_owned())),
        (
            "commandWindows".to_owned(),
            Value::String(command.to_owned()),
        ),
        ("async".to_owned(), Value::Bool(event != "SessionEnd")),
        (
            "statusMessage".to_owned(),
            Value::String(OWNER_TAG.to_owned()),
        ),
    ]);
    if event == "SessionEnd" {
        handler.insert(
            "timeout".to_owned(),
            Value::Number(SESSION_END_TIMEOUT_SECONDS.into()),
        );
    }
    json!({ "hooks": [Value::Object(handler)] })
}

fn hooks_object(document: &Value) -> Result<&Map<String, Value>> {
    let root = document
        .as_object()
        .context("hooks.json root must be an object")?;
    for key in root.keys() {
        ensure!(
            matches!(key.as_str(), "description" | "hooks"),
            "unsupported top-level hooks.json field `{key}`"
        );
    }
    if let Some(description) = root.get("description") {
        ensure!(
            description.is_null() || description.is_string(),
            "hooks.json field `description` must be a string or null"
        );
    }
    match root.get("hooks") {
        Some(hooks) => hooks
            .as_object()
            .context("hooks.json field `hooks` must be an object"),
        None => Ok(empty_map()),
    }
}

fn hooks_object_mut(document: &mut Value) -> Result<&mut Map<String, Value>> {
    let root = document
        .as_object_mut()
        .context("hooks.json root must be an object")?;
    if !root.contains_key("hooks") {
        root.insert("hooks".to_owned(), Value::Object(Map::new()));
    }
    root.get_mut("hooks")
        .and_then(Value::as_object_mut)
        .context("hooks.json field `hooks` must be an object")
}

fn empty_map() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

fn validate_hooks_shape(hooks: &Map<String, Value>) -> Result<()> {
    for event in KNOWN_HOOK_EVENTS {
        let Some(groups) = hooks.get(event) else {
            continue;
        };
        let groups = groups
            .as_array()
            .ok_or_else(|| anyhow!("hooks.{event} must be an array"))?;
        for (group_index, group) in groups.iter().enumerate() {
            let group = group
                .as_object()
                .ok_or_else(|| anyhow!("hooks.{event}[{group_index}] must be an object"))?;
            if let Some(matcher) = group.get("matcher") {
                ensure!(
                    matcher.is_null() || matcher.is_string(),
                    "hooks.{event}[{group_index}].matcher must be a string or null"
                );
            }
            let Some(handlers) = group.get("hooks") else {
                continue;
            };
            let handlers = handlers
                .as_array()
                .ok_or_else(|| anyhow!("hooks.{event}[{group_index}].hooks must be an array"))?;
            for (handler_index, handler) in handlers.iter().enumerate() {
                validate_hook_handler(handler).with_context(|| {
                    format!("invalid hooks.{event}[{group_index}].hooks[{handler_index}]")
                })?;
            }
        }
    }
    Ok(())
}

fn validate_hook_handler(handler: &Value) -> Result<()> {
    let handler = handler
        .as_object()
        .context("hook handler must be an object")?;
    let handler_type = handler
        .get("type")
        .and_then(Value::as_str)
        .context("hook handler field `type` must be a string")?;
    match handler_type {
        "command" => {
            required_string(handler, "command")?;
            ensure!(
                !(handler.contains_key("commandWindows")
                    && handler.contains_key("command_windows")),
                "hook handler cannot contain both `commandWindows` and `command_windows`"
            );
            optional_string(handler, "commandWindows")?;
            optional_string(handler, "command_windows")?;
            optional_u64(handler, "timeout")?;
            optional_bool(handler, "async")?;
            optional_string(handler, "statusMessage")?;
            optional_u64(handler, "additionalContextLimit")?;
        }
        "mcp_tool" => {
            required_string(handler, "server")?;
            required_string(handler, "tool")?;
            if let Some(input) = handler.get("input") {
                ensure!(
                    input.is_object(),
                    "hook handler field `input` must be an object"
                );
                ensure!(
                    value_is_toml_compatible(input),
                    "hook handler field `input` must not contain null or out-of-range integers"
                );
            }
            optional_u64(handler, "timeout")?;
            optional_string(handler, "statusMessage")?;
        }
        "prompt" | "agent" => {}
        other => bail!("unsupported hook handler type `{other}`"),
    }
    Ok(())
}

fn required_string(handler: &Map<String, Value>, field: &str) -> Result<()> {
    ensure!(
        handler.get(field).is_some_and(Value::is_string),
        "hook handler field `{field}` must be a string"
    );
    Ok(())
}

fn optional_string(handler: &Map<String, Value>, field: &str) -> Result<()> {
    if let Some(value) = handler.get(field) {
        ensure!(
            value.is_null() || value.is_string(),
            "hook handler field `{field}` must be a string or null"
        );
    }
    Ok(())
}

fn optional_u64(handler: &Map<String, Value>, field: &str) -> Result<()> {
    if let Some(value) = handler.get(field) {
        ensure!(
            value.is_null() || value.as_u64().is_some(),
            "hook handler field `{field}` must be a non-negative integer or null"
        );
    }
    Ok(())
}

fn optional_bool(handler: &Map<String, Value>, field: &str) -> Result<()> {
    if let Some(value) = handler.get(field) {
        ensure!(
            value.is_boolean(),
            "hook handler field `{field}` must be a boolean"
        );
    }
    Ok(())
}

fn value_is_toml_compatible(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::String(_) => true,
        Value::Number(number) => {
            number.as_i64().is_some()
                || (number.is_f64() && number.as_f64().is_some_and(f64::is_finite))
        }
        Value::Array(values) => values.iter().all(value_is_toml_compatible),
        Value::Object(values) => values.values().all(value_is_toml_compatible),
    }
}

fn inspect_event(groups: &Value, event: &str, expected_command: &str) -> Result<(usize, usize)> {
    let groups = groups
        .as_array()
        .ok_or_else(|| anyhow!("hooks.{event} must be an array"))?;
    let mut owned_count = 0;
    let mut current_count = 0;
    for (group_index, group) in groups.iter().enumerate() {
        let group = group
            .as_object()
            .ok_or_else(|| anyhow!("hooks.{event}[{group_index}] must be an object"))?;
        let Some(handlers) = group.get("hooks") else {
            continue;
        };
        let handlers = handlers
            .as_array()
            .ok_or_else(|| anyhow!("hooks.{event}[{group_index}].hooks must be an array"))?;
        for (handler_index, handler) in handlers.iter().enumerate() {
            ensure!(
                handler.is_object(),
                "hooks.{event}[{group_index}].hooks[{handler_index}] must be an object"
            );
            if is_owned_handler(handler) {
                owned_count += 1;
                if group_matches_all(group, event)
                    && is_current_handler(handler, event, expected_command)
                {
                    current_count += 1;
                }
            }
        }
    }
    Ok((owned_count, current_count))
}

fn is_owned_handler(handler: &Value) -> bool {
    handler.get("type").and_then(Value::as_str) == Some("command")
        && handler.get("statusMessage").and_then(Value::as_str) == Some(OWNER_TAG)
}

fn group_matches_all(group: &Map<String, Value>, event: &str) -> bool {
    if matches!(event, "UserPromptSubmit" | "Stop") {
        return true;
    }
    group
        .get("matcher")
        .and_then(Value::as_str)
        .is_none_or(|matcher| matcher.is_empty() || matcher == "*")
}

fn is_current_handler(handler: &Value, event: &str, expected_command: &str) -> bool {
    let expected_timeout = (event == "SessionEnd").then_some(SESSION_END_TIMEOUT_SECONDS);
    let timeout = handler.get("timeout").and_then(Value::as_u64);
    is_owned_handler(handler)
        && handler.get("command").and_then(Value::as_str) == Some(expected_command)
        && handler.get("commandWindows").and_then(Value::as_str) == Some(expected_command)
        && handler.get("async").and_then(Value::as_bool) == Some(event != "SessionEnd")
        && timeout == expected_timeout
}

fn remove_owned_handlers(groups: &mut Value, event: &str) -> Result<bool> {
    let groups = groups
        .as_array_mut()
        .ok_or_else(|| anyhow!("hooks.{event} must be an array"))?;
    let mut removed_any = false;
    groups.retain_mut(|group| {
        let Some(group_object) = group.as_object_mut() else {
            return true;
        };
        let Some(handlers_value) = group_object.get_mut("hooks") else {
            return true;
        };
        let Some(handlers) = handlers_value.as_array_mut() else {
            return true;
        };
        let old_length = handlers.len();
        handlers.retain(|handler| !is_owned_handler(handler));
        let removed_from_group = old_length != handlers.len();
        removed_any |= removed_from_group;
        !(removed_from_group && handlers.is_empty() && is_removable_owned_group(group_object))
    });

    // Refuse to install through malformed target event structures. The retain
    // pass above intentionally leaves them untouched so no data is lost.
    for (group_index, group) in groups.iter().enumerate() {
        let group = group
            .as_object()
            .ok_or_else(|| anyhow!("hooks.{event}[{group_index}] must be an object"))?;
        if let Some(handlers) = group.get("hooks") {
            let handlers = handlers
                .as_array()
                .ok_or_else(|| anyhow!("hooks.{event}[{group_index}].hooks must be an array"))?;
            for (handler_index, handler) in handlers.iter().enumerate() {
                ensure!(
                    handler.is_object(),
                    "hooks.{event}[{group_index}].hooks[{handler_index}] must be an object"
                );
            }
        }
    }
    Ok(removed_any)
}

fn is_removable_owned_group(group: &Map<String, Value>) -> bool {
    group
        .keys()
        .all(|key| matches!(key.as_str(), "hooks" | "matcher"))
        && group
            .get("matcher")
            .is_none_or(|matcher| matcher.as_str() == Some(""))
}

fn read_document_for_update(path: &Path) -> Result<Option<Value>> {
    let source = match fs::read(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let document = serde_json::from_slice(&source)
        .with_context(|| format!("refusing to modify malformed {}", path.display()))?;
    // Validate official structural fields before mutation. Unknown hook events
    // and fields within the hooks object remain untouched for forward compatibility.
    validate_hooks_shape(hooks_object(&document)?)?;
    Ok(Some(document))
}

fn write_json_atomically(path: &Path, document: &Value) -> Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(document).context("failed to serialize hooks.json")?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let (temporary_path, mut temporary_file) = create_temporary_file(path, 0o600)?;
    let result = (|| -> Result<()> {
        temporary_file.write_all(bytes)?;
        temporary_file.sync_all()?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary_path, metadata.permissions())?;
        }
        drop(temporary_file);
        replace_file(&temporary_path, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_temporary_file(path: &Path, _unix_mode: u32) -> Result<(PathBuf, File)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("raylander");
    for _ in 0..128 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = parent.join(format!(
            ".{name}.raylander.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(_unix_mode);
        }
        match options.open(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create temporary file in {}", parent.display())
                });
            }
        }
    }
    bail!(
        "failed to allocate a unique temporary file in {}",
        parent.display()
    )
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination).with_context(|| {
        format!(
            "failed to atomically replace {} with {}",
            destination.display(),
            source.display()
        )
    })
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // Same-directory MoveFileExW with REPLACE_EXISTING is the Windows atomic
    // replacement primitive; WRITE_THROUGH asks the system to flush the move.
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| format!("failed to atomically replace {}", destination.display()))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all().map_err(Into::into)
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

fn ensure_auth_token_at(path: &Path) -> Result<String> {
    if let Some(token) = read_auth_token_at(path)? {
        tighten_token_permissions(path)?;
        return Ok(token);
    }
    let parent = path
        .parent()
        .context("authentication token path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    tighten_directory_permissions(parent)?;

    // Create the complete token as a private temporary file, then use a hard
    // link as an atomic create-if-absent operation. This avoids exposing a
    // partially written token and avoids replacing a concurrently created one.
    for _ in 0..8 {
        let mut random = [0_u8; TOKEN_BYTES];
        fill_secure_random(&mut random)?;
        let token = hex_encode(&random);
        let (temporary_path, mut temporary_file) = create_temporary_file(path, 0o600)?;
        let attempt = (|| -> Result<bool> {
            temporary_file.write_all(token.as_bytes())?;
            temporary_file.sync_all()?;
            drop(temporary_file);
            match fs::hard_link(&temporary_path, path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error).with_context(|| {
                    format!(
                        "failed to persist authentication token at {}",
                        path.display()
                    )
                }),
            }
        })();
        let _ = fs::remove_file(&temporary_path);
        if attempt? {
            tighten_token_permissions(path)?;
            sync_parent_directory(parent)?;
            return Ok(token);
        }
        if let Some(existing) = read_auth_token_at(path)? {
            tighten_token_permissions(path)?;
            return Ok(existing);
        }
    }
    bail!(
        "authentication token could not be created at {}",
        path.display()
    )
}

fn read_auth_token_at(path: &Path) -> Result<Option<String>> {
    let token = match fs::read_to_string(path) {
        Ok(token) => token,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    ensure!(
        token.len() == TOKEN_HEX_LENGTH
            && token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "authentication token at {} is malformed",
        path.display()
    );
    Ok(Some(token))
}

#[cfg(unix)]
fn tighten_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn tighten_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn tighten_token_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn tighten_token_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn fill_secure_random(bytes: &mut [u8]) -> Result<()> {
    use std::ffi::c_void;

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, length: u32, flags: u32)
        -> i32;
    }

    let length = u32::try_from(bytes.len()).context("random token request is too large")?;
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            length,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    ensure!(
        status == 0,
        "BCryptGenRandom failed with NTSTATUS {status:#x}"
    );
    Ok(())
}

#[cfg(not(windows))]
fn fill_secure_random(bytes: &mut [u8]) -> Result<()> {
    File::open("/dev/urandom")
        .context("failed to open operating-system random source")?
        .read_exact(bytes)
        .context("failed to read operating-system random source")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn constant_time_token_eq(expected: &[u8], candidate: &[u8]) -> bool {
    if expected.len() != TOKEN_HEX_LENGTH || candidate.len() != TOKEN_HEX_LENGTH {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in expected.iter().zip(candidate) {
        difference |= left ^ right;
    }
    difference == 0
}

fn post_payload_with_token(address: SocketAddr, payload: &[u8], token: &str) -> Result<()> {
    ensure!(
        address.ip().is_loopback(),
        "refusing to send Codex hook token to non-loopback address {address}"
    );
    ensure!(
        address.port() != 0,
        "daemon address must use a non-zero port"
    );
    ensure!(
        token.len() == TOKEN_HEX_LENGTH,
        "authentication token has an invalid length"
    );
    let endpoint = format!("http://{address}{DAEMON_NOTIFICATION_PATH}");
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("failed to create Codex hook HTTP client")?;
    let response = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(CODEX_HOOK_TOKEN_HEADER, token)
        .body(payload.to_vec())
        .send()
        .with_context(|| format!("failed to notify Raylander daemon at {endpoint}"))?;
    ensure!(
        response.status().is_success(),
        "Raylander daemon rejected Codex hook notification with HTTP {}",
        response.status()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };

    use super::*;

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "raylander-codex-hooks-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn executable(name: &str) -> PathBuf {
        env::temp_dir().join(name).join("raylander.exe")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn owned_handlers<'a>(document: &'a Value, event: &str) -> Vec<&'a Value> {
        document["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
            .filter(|handler| is_owned_handler(handler))
            .collect()
    }

    #[test]
    fn install_preserves_unrelated_hooks_and_fields() {
        let directory = TestDirectory::new();
        let path = directory.path().join("hooks.json");
        fs::write(
            &path,
            br#"{
                "description": "user hooks",
                "hooks": {
                    "SessionStart": [{"matcher":"startup","hooks":[{"type":"command","command":"user-script"}]}],
                    "PreCompact": [{"hooks":[{"type":"command","command":"compact-script"}]}],
                    "FutureEvent": {"keep": true},
                    "state": {"some-user-hook": {"enabled": false}}
                }
            }"#,
        )
        .unwrap();
        let executable = executable("current");

        let installed = install_at(directory.path(), &executable).unwrap();

        assert_eq!(installed.state, HookState::Installed);
        let document = read_json(&path);
        assert_eq!(document["description"], "user hooks");
        assert_eq!(document["hooks"]["FutureEvent"]["keep"], true);
        assert_eq!(
            document["hooks"]["PreCompact"][0]["hooks"][0]["command"],
            "compact-script"
        );
        assert_eq!(
            document["hooks"]["state"]["some-user-hook"]["enabled"],
            false
        );
        assert_eq!(
            document["hooks"]["SessionStart"].as_array().unwrap().len(),
            2
        );
        for event in HOOK_EVENTS {
            let handlers = owned_handlers(&document, event);
            assert_eq!(handlers.len(), 1, "event {event}");
            assert_eq!(handlers[0]["async"], event != "SessionEnd");
            if event == "SessionEnd" {
                assert_eq!(handlers[0]["timeout"], SESSION_END_TIMEOUT_SECONDS);
            } else {
                assert!(handlers[0].get("timeout").is_none());
            }
            let command = handlers[0]["commandWindows"].as_str().unwrap();
            assert!(command.contains(executable.to_str().unwrap()));
        }
    }

    #[test]
    fn install_is_idempotent_and_refreshes_stale_paths() {
        let directory = TestDirectory::new();
        let old_executable = executable("old");
        let current_executable = executable("current");
        install_at(directory.path(), &old_executable).unwrap();
        assert_eq!(
            status_at(directory.path(), &current_executable)
                .unwrap()
                .state,
            HookState::Stale
        );

        install_at(directory.path(), &current_executable).unwrap();
        let path = directory.path().join("hooks.json");
        let once = fs::read(&path).unwrap();
        install_at(directory.path(), &current_executable).unwrap();
        let twice = fs::read(&path).unwrap();

        assert_eq!(once, twice);
        assert_eq!(
            status_at(directory.path(), &current_executable)
                .unwrap()
                .state,
            HookState::Installed
        );
        let document: Value = serde_json::from_slice(&twice).unwrap();
        for event in HOOK_EVENTS {
            assert_eq!(owned_handlers(&document, event).len(), 1);
        }
    }

    #[test]
    fn current_installation_is_left_byte_for_byte_unchanged() {
        let directory = TestDirectory::new();
        let executable = executable("current");
        install_at(directory.path(), &executable).unwrap();
        let path = directory.path().join("hooks.json");
        let mut document = read_json(&path);
        document["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"command", "command":"user-hook"}));
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();

        assert_eq!(
            install_at(directory.path(), &executable).unwrap().state,
            HookState::Installed
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn status_detects_owned_handler_with_restrictive_matcher() {
        let directory = TestDirectory::new();
        let executable = executable("current");
        install_at(directory.path(), &executable).unwrap();
        let path = directory.path().join("hooks.json");
        let mut document = read_json(&path);
        document["hooks"]["SessionStart"][0]["matcher"] = Value::String("startup".to_owned());
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let status = status_at(directory.path(), &executable).unwrap();

        assert_eq!(status.state, HookState::Stale);
        assert_eq!(status.stale_events, ["SessionStart"]);
    }

    #[test]
    fn uninstall_removes_only_owned_handlers() {
        let directory = TestDirectory::new();
        let executable = executable("current");
        install_at(directory.path(), &executable).unwrap();
        let path = directory.path().join("hooks.json");
        let mut document = read_json(&path);
        document["hooks"]["PostToolUse"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"command", "command":"user-hook"}));
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let removed = uninstall_at(directory.path(), &executable).unwrap();

        assert_eq!(removed.state, HookState::NotInstalled);
        let document = read_json(&path);
        assert_eq!(
            document["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "user-hook"
        );
        for event in HOOK_EVENTS {
            assert!(owned_handlers(&document, event).is_empty());
        }
    }

    #[test]
    fn unsupported_top_level_data_is_preserved_by_refusing_to_modify_it() {
        let directory = TestDirectory::new();
        let path = directory.path().join("hooks.json");
        let source = br#"{"description":"user hooks","futureField":{"keep":true},"hooks":{}}"#;
        fs::write(&path, source).unwrap();
        let executable = executable("current");

        assert_eq!(
            status_at(directory.path(), &executable).unwrap().state,
            HookState::Malformed
        );
        assert!(install_at(directory.path(), &executable).is_err());
        assert!(uninstall_at(directory.path(), &executable).is_err());
        assert_eq!(fs::read(path).unwrap(), source);
    }

    #[test]
    fn malformed_file_is_never_replaced() {
        let directory = TestDirectory::new();
        let path = directory.path().join("hooks.json");
        let malformed = b"{ definitely not json";
        fs::write(&path, malformed).unwrap();
        let executable = executable("current");

        assert_eq!(
            status_at(directory.path(), &executable).unwrap().state,
            HookState::Malformed
        );
        assert!(install_at(directory.path(), &executable).is_err());
        assert!(uninstall_at(directory.path(), &executable).is_err());
        assert_eq!(fs::read(&path).unwrap(), malformed);

        for malformed_document in [
            br#"{"hooks":{"PreCompact":"invalid"}}"#.as_slice(),
            br#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command"}]}]}}"#.as_slice(),
            br#"{"hooks":{"PostToolUse":[{"matcher":42,"hooks":[]}]}}"#.as_slice(),
        ] {
            fs::write(&path, malformed_document).unwrap();
            assert_eq!(
                status_at(directory.path(), &executable).unwrap().state,
                HookState::Malformed
            );
            assert!(install_at(directory.path(), &executable).is_err());
            assert_eq!(fs::read(&path).unwrap(), malformed_document);
        }
    }

    #[test]
    fn oversized_payload_is_rejected_before_network_access() {
        let payload = vec![b'x'; MAX_HOOK_PAYLOAD_BYTES as usize + 1];
        let address = "127.0.0.1:1".parse().unwrap();
        let error = forward_reader_to_daemon(payload.as_slice(), address).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn token_is_random_persistent_and_validated() {
        let directory = TestDirectory::new();
        let path = directory.path().join("private").join("token");

        let first = ensure_auth_token_at(&path).unwrap();
        let second = ensure_auth_token_at(&path).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.len(), TOKEN_HEX_LENGTH);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(constant_time_token_eq(first.as_bytes(), second.as_bytes()));
        assert!(!constant_time_token_eq(first.as_bytes(), b"wrong"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn authenticated_payload_is_posted_only_to_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let token = "a".repeat(TOKEN_HEX_LENGTH);
        let expected_token = token.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut received = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                received.extend_from_slice(&chunk[..count]);
                let Some(header_end) = received.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&received[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|length| length.parse::<usize>().ok())
                    })
                    .unwrap();
                if received.len() >= header_end + 4 + content_length {
                    assert!(
                        headers.starts_with(&format!("POST {DAEMON_NOTIFICATION_PATH} HTTP/1.1"))
                    );
                    assert!(headers.lines().any(|line| {
                        line.eq_ignore_ascii_case(&format!(
                            "{CODEX_HOOK_TOKEN_HEADER}: {expected_token}"
                        ))
                    }));
                    assert_eq!(
                        &received[header_end + 4..header_end + 4 + content_length],
                        br#"{"hookEventName":"Stop"}"#
                    );
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        post_payload_with_token(address, br#"{"hookEventName":"Stop"}"#, &token).unwrap();
        server.join().unwrap();
        let non_loopback: SocketAddr = "192.0.2.1:8030".parse().unwrap();
        assert!(post_payload_with_token(non_loopback, b"{}", &token).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn high_level_operations_are_noops_off_windows() {
        assert_eq!(install().unwrap().state, HookState::Unsupported);
        assert_eq!(status().unwrap().state, HookState::Unsupported);
        assert_eq!(uninstall().unwrap().state, HookState::Unsupported);
    }
}

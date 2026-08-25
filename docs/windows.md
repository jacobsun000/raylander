# Raylander for Windows

## Start

1. Extract the ZIP to a permanent folder and keep `raylander.exe` beside `raylander-tray.exe`.
2. Double-click `raylander-tray.exe`.
3. If Codex asks you to trust the Raylander lifecycle hooks, review and approve them.
4. Open the dock from the Raylander notification-area menu, or browse to <http://127.0.0.1:8030>.

Raylander runs in the background without a console window. Starting it a second time asks the existing tray instance to open its dock. Use **Exit** in the notification-area menu to remove the icon and stop the server gracefully.

The tray uses a stock Windows application icon in this initial ZIP distribution.

## Notification-area menu

- **Open Dock** opens the local web interface.
- **Open Config Folder** creates and opens `%APPDATA%\Raylander`.
- **Reinstall Hooks** repairs Raylander's Codex hook registrations after moving the executables.
- **Exit** removes the icon and stops the dock server.

## Configuration

The Windows configuration file is:

```text
%APPDATA%\Raylander\config.toml
```

Copy `config.example.toml` there to customize the bind address, port, components, or layout. Restart Raylander after editing it. The default `0.0.0.0:8030` listener is reachable from the local network and has no authentication; do not expose it to the public internet.

The Codex helper sends its authentication token only to a loopback endpoint. Wildcard bind addresses such as the default are mapped to loopback for this purpose. If you bind only to a specific non-loopback address, the dock still runs, but automatic hook installation is skipped and Windows agent status cannot be delivered with the current single-listener design.

## Command-line diagnostics

`raylander.exe` remains a console application:

```powershell
.\raylander.exe daemon
.\raylander.exe hooks status
.\raylander.exe hooks install
.\raylander.exe hooks remove
```

When `[components.agents]` is enabled, `daemon` and the tray attempt to install or repair the Codex hooks after the web server is available. Hook installation is best effort: a malformed or unwritable Codex configuration does not stop the dock, and `hooks install` reports the error for troubleshooting. The internal `codex-hook` command is invoked by Codex and reads one lifecycle event as JSON from standard input; it exits successfully without forwarding when either the dock or agents component is disabled.

Codex stores user hooks under `%CODEX_HOME%` when set, otherwise under `%USERPROFILE%\.codex`. Raylander preserves unrelated hook entries. Removing Raylander's hooks does not alter other integrations.

## Build a ZIP

From a PowerShell checkout with Rust 1.90 or newer:

```powershell
cargo build --release --locked --bins
.\scripts\package-windows.ps1
```

The archive is written to `dist\raylander-windows-x86_64.zip`.

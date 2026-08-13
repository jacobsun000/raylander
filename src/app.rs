use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::mpsc::Receiver,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use gtk::{gio, glib, prelude::*};

use crate::{
    audio::{AudioObserver, VolumeState, WpctlBackend},
    cli::{
        Cli, Command, InputCommand, MediaCommand, MicCommand, NotificationsCommand,
        PackagesCommand, PreviewColorScheme, SidebarCommand, SidebarSection, ToggleState,
        VolumeCommand,
    },
    config::{self, Config},
    notification_service::{NotificationService, NotificationServiceEvent},
    notifications::{Notification, NotificationCenter, persist_atomically},
    shell::{ShellState, ShellUi},
    shell_sources::{ShellObserver, ShellUpdate},
    theme::ThemeManager,
    tray::{TrayEvent, TrayState},
    tray_service::TrayService,
    volume_osd::VolumeOsd,
    web_server::WebServer,
};

const APP_ID: &str = "io.raylander.Raylander";

pub fn run() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("raylander=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    if let Command::Preview {
        duration,
        output,
        color_scheme,
    } = cli.command
    {
        return run_preview(duration, output, color_scheme);
    }

    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let runtime: Rc<RefCell<Option<Rc<RefCell<Runtime>>>>> = Rc::new(RefCell::new(None));

    app.connect_startup({
        let runtime = Rc::clone(&runtime);
        move |app| {
            let state = Runtime::new(app);
            Runtime::start_event_poll(&state);
            *runtime.borrow_mut() = Some(state);
        }
    });

    app.connect_command_line({
        let runtime = Rc::clone(&runtime);
        move |app, command_line| {
            let args = command_line.arguments();
            let parsed = Cli::try_parse_from(args);
            let result = match parsed {
                Ok(cli) => runtime
                    .borrow()
                    .as_ref()
                    .context("application runtime is not initialized")
                    .and_then(|runtime| runtime.borrow_mut().handle(cli.command, app)),
                Err(error) => {
                    let rendered = error.to_string();
                    if error.use_stderr() {
                        command_line.printerr_literal(&rendered);
                    } else {
                        command_line.print_literal(&rendered);
                    }
                    if !command_line.is_remote() {
                        app.quit();
                    }
                    return if error.use_stderr() { 2 } else { 0 }.into();
                }
            };
            match result {
                Ok(()) => 0.into(),
                Err(error) => {
                    command_line.printerr_literal(&format!("raylander: {error:#}\n"));
                    1.into()
                }
            }
        }
    });

    app.connect_shutdown(move |_| {
        runtime.borrow_mut().take();
    });
    app.run()
}

/// Run previews in this process so `cargo run -- preview` always displays the
/// just-compiled UI, even while an older daemon owns the main application ID.
fn run_preview(
    duration: Duration,
    output: Option<PathBuf>,
    color_scheme: Option<PreviewColorScheme>,
) -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.raylander.Raylander.Preview")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let preview = Rc::new(RefCell::new(None));

    app.connect_startup({
        let preview = Rc::clone(&preview);
        move |app| {
            let config = config::load().unwrap_or_else(|error| {
                tracing::error!(%error, "invalid configuration; using defaults");
                Config::default()
            });
            let theme = ThemeManager::new(&config);
            if let Some(scheme) = color_scheme {
                theme.preview_scheme(match scheme {
                    PreviewColorScheme::Light => crate::config::ColorScheme::Light,
                    PreviewColorScheme::Dark => crate::config::ColorScheme::Dark,
                });
            }
            let shell = ShellUi::new(app, &config);
            let osd = VolumeOsd::new(app, &config.components.volume_osd);

            if let Some(path) = output.clone() {
                shell.render_demo(path, app.clone());
            } else {
                shell.show();
                shell.show_demo(app);
                osd.demo(duration);
                let app = app.clone();
                glib::timeout_add_local_once(duration, move || app.quit());
            }
            *preview.borrow_mut() = Some((theme, shell, osd));
        }
    });
    app.connect_shutdown(move |_| {
        preview.borrow_mut().take();
    });
    app.run_with_args(&["raylander-preview"])
}

struct Runtime {
    _hold: gio::ApplicationHoldGuard,
    app: gtk::Application,
    config: Config,
    backend: WpctlBackend,
    osd: VolumeOsd,
    theme: ThemeManager,
    shell: ShellUi,
    shell_state: ShellState,
    _shell_observer: ShellObserver,
    shell_events: Receiver<ShellUpdate>,
    notifications: NotificationCenter,
    notification_service: Option<NotificationService>,
    notification_events: Option<Receiver<NotificationServiceEvent>>,
    _tray_service: Option<TrayService>,
    tray_events: Option<Receiver<TrayEvent>>,
    tray_state: TrayState,
    observer: Option<AudioObserver>,
    observer_events: Option<Receiver<()>>,
    last_state: Option<VolumeState>,
    web_server: Option<WebServer>,
}

impl Runtime {
    fn new(app: &gtk::Application) -> Rc<RefCell<Self>> {
        let hold = app.hold();
        let config = config::load().unwrap_or_else(|error| {
            tracing::error!(%error, "invalid configuration; using defaults");
            Config::default()
        });
        let backend = WpctlBackend;
        let last_state = backend.current().ok();
        let theme = ThemeManager::new(&config);
        let osd = VolumeOsd::new(app, &config.components.volume_osd);
        let shell = ShellUi::new(app, &config);
        let notifications = load_notification_center(config.components.notifications.history_limit);
        let mut shell_state = ShellState::default();
        if let Some(state) = last_state {
            shell_state.audio = audio_label(state);
        }
        shell_state.notification_count = notifications.history().count();
        shell.update(&shell_state);
        let (shell_observer, shell_events) = ShellObserver::start();
        let (notification_service, notification_events) = if config.components.notifications.enabled
        {
            match NotificationService::start(config.components.notifications.history_limit) {
                Ok((service, receiver)) => (Some(service), Some(receiver)),
                Err(error) => {
                    tracing::warn!(%error, "notification service unavailable; stop Dunst to let Raylander own it");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        let (tray_service, tray_events) = match TrayService::start() {
            Ok((service, receiver)) => (Some(service), Some(receiver)),
            Err(error) => {
                tracing::warn!(%error, "system tray service unavailable");
                (None, None)
            }
        };
        let (observer, observer_events) = if config.components.volume_osd.observe_external {
            let (observer, receiver) = AudioObserver::start();
            (Some(observer), Some(receiver))
        } else {
            (None, None)
        };
        let web_server = if config.components.external_dock.enabled {
            match WebServer::start(&config.components.external_dock) {
                Ok(server) => Some(server),
                Err(error) => {
                    tracing::warn!(%error, "external dock server unavailable");
                    None
                }
            }
        } else {
            None
        };
        Rc::new(RefCell::new(Self {
            _hold: hold,
            app: app.clone(),
            config,
            backend,
            osd,
            theme,
            shell,
            shell_state,
            _shell_observer: shell_observer,
            shell_events,
            notifications,
            notification_service,
            notification_events,
            _tray_service: tray_service,
            tray_events,
            tray_state: TrayState::default(),
            observer,
            observer_events,
            last_state,
            web_server,
        }))
    }

    fn start_event_poll(runtime: &Rc<RefCell<Self>>) {
        let weak: Weak<RefCell<Self>> = Rc::downgrade(runtime);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let Some(runtime) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            runtime.borrow_mut().poll_observer();
            glib::ControlFlow::Continue
        });
    }

    fn handle(&mut self, command: Command, app: &gtk::Application) -> Result<()> {
        match command {
            Command::Daemon => Ok(()),
            Command::Volume { command } => self.handle_volume(command),
            Command::Mic { command } => self.handle_microphone(command),
            Command::Sidebar { command } => {
                self.handle_sidebar(command);
                Ok(())
            }
            Command::Notifications { command } => self.handle_notifications(command),
            Command::Media { command } => {
                self.handle_media(command);
                Ok(())
            }
            Command::Input { command } => {
                self.handle_input(command);
                Ok(())
            }
            Command::Packages { command } => {
                self.handle_packages(command);
                Ok(())
            }
            Command::Preview { .. } => {
                anyhow::bail!("preview must run in the local process")
            }
            Command::Reload => self.reload(),
            Command::Quit => {
                app.quit();
                Ok(())
            }
        }
    }

    fn handle_volume(&mut self, command: VolumeCommand) -> Result<()> {
        let config = &self.config.components.volume_osd;
        let state = self
            .backend
            .apply(command, config.step_percent, config.limit_percent)?;
        self.last_state = Some(state);
        self.shell_state.audio = audio_label(state);
        self.shell.update(&self.shell_state);
        Ok(())
    }

    fn handle_microphone(&mut self, command: MicCommand) -> Result<()> {
        let config = &self.config.components.volume_osd;
        self.backend
            .apply_microphone(command, config.step_percent, config.limit_percent)?;
        Ok(())
    }

    fn handle_sidebar(&self, command: SidebarCommand) {
        match command {
            SidebarCommand::Show { section } => {
                self.shell.show_sidebar(section.map(sidebar_target))
            }
            SidebarCommand::Hide => self.shell.hide_sidebar(),
            SidebarCommand::Toggle { section } => {
                self.shell.toggle_sidebar(section.map(sidebar_target));
            }
        }
    }

    fn handle_notifications(&mut self, command: NotificationsCommand) -> Result<()> {
        match command {
            NotificationsCommand::Clear => self.notifications.clear_history(),
            NotificationsCommand::Dnd { state } => {
                let enabled = match state {
                    ToggleState::On => true,
                    ToggleState::Off => false,
                    ToggleState::Toggle => !self.notifications.dnd(),
                };
                self.notifications.set_dnd(enabled);
                if let Some(service) = &self.notification_service {
                    service.set_dnd(enabled)?;
                }
            }
        }
        self.shell_state.notification_count = self.notifications.history().count();
        self.shell.update(&self.shell_state);
        persist_notifications(&self.notifications)?;
        Ok(())
    }

    fn handle_media(&self, command: MediaCommand) {
        let action = match command {
            MediaCommand::PlayPause => crate::media::MediaAction::PlayPause,
            MediaCommand::Next => crate::media::MediaAction::Next,
            MediaCommand::Previous => crate::media::MediaAction::Previous,
        };
        std::thread::spawn(move || {
            if let Err(error) = crate::media::PlayerctlBackend.apply(None, action) {
                tracing::warn!(%error, "media action failed");
            }
        });
    }

    fn handle_input(&self, command: InputCommand) {
        std::thread::spawn(move || {
            use crate::input_method::{Fcitx5Backend, InputMethodAction};
            let backend = Fcitx5Backend;
            let action = match command {
                InputCommand::Toggle => InputMethodAction::Toggle,
                InputCommand::Next => match backend.current().ok().and_then(|state| state.name) {
                    Some(name) if name == "pinyin" => {
                        InputMethodAction::Select("keyboard-us-colemak".into())
                    }
                    _ => InputMethodAction::Select("pinyin".into()),
                },
            };
            if let Err(error) = backend.apply(&action) {
                tracing::warn!(%error, "input-method action failed");
            }
        });
    }

    fn handle_packages(&self, command: PackagesCommand) {
        match command {
            PackagesCommand::Refresh => self._shell_observer.refresh_packages(),
        }
    }

    fn poll_observer(&mut self) {
        self.poll_audio_observer();
        self.poll_notifications();
        self.poll_tray();
        let mut changed = false;
        while let Ok(update) = self.shell_events.try_recv() {
            match update {
                ShellUpdate::ActiveApp(value) => self.shell_state.active_app = value,
                ShellUpdate::Media(value) => self.shell_state.media = value,
                ShellUpdate::Network(value) => self.shell_state.network = value,
                ShellUpdate::Packages(value) => self.shell_state.packages = Some(value),
                ShellUpdate::InputMethod(value) => self.shell_state.input_method = value,
                ShellUpdate::Microphone(value) => self.shell_state.microphone = value,
                ShellUpdate::Workspaces(value) => self.shell_state.workspaces = value,
                ShellUpdate::SystemHealth(value) => self.shell_state.system_health = value,
                ShellUpdate::Storage(value) => self.shell_state.storage = value,
                ShellUpdate::Display(value) => self.shell_state.display = value,
                ShellUpdate::Power(value) => self.shell_state.power = value,
                ShellUpdate::Privacy(value) => self.shell_state.privacy = value,
                ShellUpdate::Bluetooth(value) => self.shell_state.bluetooth = value,
            }
            changed = true;
        }
        if changed {
            self.shell.update(&self.shell_state);
        }
    }

    fn poll_notifications(&mut self) {
        let events: Vec<_> = self
            .notification_events
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        if events.is_empty() {
            return;
        }
        let mut history_changed = false;
        for event in events {
            match event {
                NotificationServiceEvent::Notified(outcome) => {
                    let notification = outcome.notification;
                    let request = crate::notifications::NotificationRequest {
                        app_name: notification.app_name.clone(),
                        replaces_id: if outcome.replaced { notification.id } else { 0 },
                        app_icon: notification.app_icon.clone(),
                        summary: notification.summary.clone(),
                        body: notification.body.clone(),
                        actions: notification.actions.clone(),
                        hints: notification.hints.clone(),
                        timeout: notification.timeout,
                        received_at_ms: notification.received_at_ms,
                    };
                    if let Err(error) = self.notifications.notify(request) {
                        tracing::warn!(?error, "failed to record notification history");
                    }
                    if outcome.should_present {
                        self.shell.show_notification(&notification);
                    }
                    history_changed = true;
                }
                NotificationServiceEvent::Closed(closed) => {
                    self.notifications
                        .close(closed.notification.id, closed.reason);
                }
                NotificationServiceEvent::ActionInvoked(_) => {}
            }
        }
        if history_changed {
            self.shell_state.notification_count = self.notifications.history().count();
            self.shell.update(&self.shell_state);
            if let Err(error) = persist_notifications(&self.notifications) {
                tracing::warn!(%error, "failed to persist notification history");
            }
        }
    }

    fn poll_tray(&mut self) {
        let events: Vec<_> = self
            .tray_events
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        if events.is_empty() {
            return;
        }
        for event in events {
            self.tray_state.apply(event);
        }
        self.shell_state.tray_icons = self
            .tray_state
            .visible_items()
            .map(|item| {
                item.icon
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "application-x-executable-symbolic".to_owned())
            })
            .collect();
        self.shell.update(&self.shell_state);
    }

    fn poll_audio_observer(&mut self) {
        let Some(receiver) = &self.observer_events else {
            return;
        };
        let mut changed = false;
        while receiver.try_recv().is_ok() {
            changed = true;
        }
        if !changed {
            return;
        }
        match self.backend.current() {
            Ok(state) if self.last_state != Some(state) => {
                self.last_state = Some(state);
                self.shell_state.audio = audio_label(state);
                self.shell.update(&self.shell_state);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to refresh observed volume"),
        }
    }

    fn reload(&mut self) -> Result<()> {
        let new_config = config::load()?;
        self.theme.reload(&new_config)?;
        self.osd.apply_config(&new_config.components.volume_osd);
        let new_shell = ShellUi::new(&self.app, &new_config);
        new_shell.update(&self.shell_state);
        self.shell = new_shell;
        if new_config.components.volume_osd.observe_external != self.observer.is_some() {
            if new_config.components.volume_osd.observe_external {
                let (observer, receiver) = AudioObserver::start();
                self.observer = Some(observer);
                self.observer_events = Some(receiver);
            } else {
                self.observer = None;
                self.observer_events = None;
            }
        }
        if new_config.components.notifications.history_limit
            != self.config.components.notifications.history_limit
        {
            let history: Vec<_> = self.notifications.history().cloned().collect();
            let mut center =
                NotificationCenter::new(new_config.components.notifications.history_limit);
            center.restore_history(history);
            self.notifications = center;
        }
        if new_config.components.external_dock != self.config.components.external_dock {
            self.web_server = None;
            if new_config.components.external_dock.enabled {
                match WebServer::start(&new_config.components.external_dock) {
                    Ok(server) => self.web_server = Some(server),
                    Err(error) => {
                        tracing::warn!(%error, "external dock server remains unavailable")
                    }
                }
            }
        } else if new_config.components.external_dock.enabled && self.web_server.is_none() {
            match WebServer::start(&new_config.components.external_dock) {
                Ok(server) => self.web_server = Some(server),
                Err(error) => tracing::warn!(%error, "external dock server remains unavailable"),
            }
        }
        if !new_config.components.notifications.enabled {
            self.notification_events = None;
            self.notification_service = None;
        } else if self.notification_service.is_none() {
            match NotificationService::start(new_config.components.notifications.history_limit) {
                Ok((service, receiver)) => {
                    self.notification_service = Some(service);
                    self.notification_events = Some(receiver);
                }
                Err(error) => tracing::warn!(%error, "notification service remains unavailable"),
            }
        }
        self.config = new_config;
        tracing::info!(path = %config::config_path().display(), "configuration reloaded");
        Ok(())
    }
}

fn audio_label(state: VolumeState) -> String {
    if state.muted {
        "Muted".into()
    } else {
        format!("{}%", state.percent)
    }
}

fn sidebar_target(section: SidebarSection) -> crate::shell::SidebarTarget {
    match section {
        SidebarSection::Controls => crate::shell::SidebarTarget::QuickSettings,
        SidebarSection::Playing => crate::shell::SidebarTarget::Playing,
        SidebarSection::Calendar => crate::shell::SidebarTarget::Calendar,
        SidebarSection::Packages => crate::shell::SidebarTarget::Packages,
        SidebarSection::Notifications => crate::shell::SidebarTarget::Notifications,
    }
}

fn notification_history_path() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(path).join("raylander/notifications.json");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/state/raylander/notifications.json")
}

fn load_notification_center(limit: usize) -> NotificationCenter {
    let mut center = NotificationCenter::new(limit);
    let path = notification_history_path();
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Vec<Notification>>(&bytes) {
            Ok(history) => center.restore_history(history),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "invalid notification history")
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to read notification history")
        }
    }
    center
}

fn persist_notifications(center: &NotificationCenter) -> Result<()> {
    let history: Vec<_> = center.history().cloned().collect();
    let encoded = serde_json::to_vec(&history).context("failed to encode notification history")?;
    let path = notification_history_path();
    persist_atomically(&path, &encoded)
        .with_context(|| format!("failed to persist {}", path.display()))
}

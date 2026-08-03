use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::mpsc::Receiver,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use gtk::{gio, glib, prelude::*};

use crate::{
    audio::{AudioObserver, VolumeState, WpctlBackend},
    cli::{Cli, Command, VolumeCommand},
    config::{self, Config},
    theme::ThemeManager,
    volume_osd::VolumeOsd,
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

struct Runtime {
    _hold: gio::ApplicationHoldGuard,
    config: Config,
    backend: WpctlBackend,
    osd: VolumeOsd,
    theme: ThemeManager,
    observer: Option<AudioObserver>,
    observer_events: Option<Receiver<()>>,
    last_state: Option<VolumeState>,
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
        let (observer, observer_events) = if config.components.volume_osd.observe_external {
            let (observer, receiver) = AudioObserver::start();
            (Some(observer), Some(receiver))
        } else {
            (None, None)
        };
        Rc::new(RefCell::new(Self {
            _hold: hold,
            config,
            backend,
            osd,
            theme,
            observer,
            observer_events,
            last_state,
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
            Command::Demo { duration, output } => {
                if let Some(path) = output {
                    self.render_demo(path, app);
                } else {
                    self.demo_components(duration);
                    let app = app.clone();
                    glib::timeout_add_local_once(duration, move || app.quit());
                }
                Ok(())
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
        self.osd.show(state);
        Ok(())
    }

    /// Keep demo orchestration here so each future component can contribute its
    /// own representative state without coupling components to the CLI.
    fn demo_components(&self, duration: Duration) {
        self.osd.demo(duration);
    }

    fn render_demo(&self, path: std::path::PathBuf, app: &gtk::Application) {
        self.osd.render_demo(path, app.clone());
    }

    fn poll_observer(&mut self) {
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
                self.osd.show(state);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to refresh observed volume"),
        }
    }

    fn reload(&mut self) -> Result<()> {
        let new_config = config::load()?;
        self.theme.reload(&new_config)?;
        self.osd.apply_config(&new_config.components.volume_osd);
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
        self.config = new_config;
        tracing::info!(path = %config::config_path().display(), "configuration reloaded");
        Ok(())
    }
}

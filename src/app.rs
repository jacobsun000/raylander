use std::{cell::RefCell, rc::Rc};

use anyhow::{Context, Result};
use clap::Parser;
use gio::{Application, ApplicationFlags, glib, prelude::*};

use crate::{
    cli::{Cli, Command},
    config::{self, Config},
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

    let app = Application::builder()
        .application_id(APP_ID)
        .flags(ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let runtime: Rc<RefCell<Option<Runtime>>> = Rc::new(RefCell::new(None));

    app.connect_startup({
        let runtime = Rc::clone(&runtime);
        move |app| match Runtime::new(app) {
            Ok(state) => *runtime.borrow_mut() = Some(state),
            Err(error) => {
                tracing::error!(%error, "failed to start daemon");
                app.quit();
            }
        }
    });

    app.connect_command_line({
        let runtime = Rc::clone(&runtime);
        move |app, command_line| {
            let parsed = Cli::try_parse_from(command_line.arguments());
            match parsed {
                Ok(Cli {
                    command: Command::Daemon,
                }) => {
                    if runtime.borrow().is_some() {
                        0.into()
                    } else {
                        command_line.printerr_literal("raylander: daemon failed to initialize\n");
                        1.into()
                    }
                }
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
                    if error.use_stderr() { 2 } else { 0 }.into()
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
    _web_server: WebServer,
}

impl Runtime {
    fn new(app: &Application) -> Result<Self> {
        let config = config::load().unwrap_or_else(|error| {
            tracing::error!(%error, "invalid configuration; using defaults");
            Config::default()
        });
        anyhow::ensure!(
            config.components.external_dock.enabled,
            "external dock is disabled in configuration"
        );
        let web_server = WebServer::start(&config.components.external_dock)
            .context("external dock server unavailable")?;

        Ok(Self {
            _hold: app.hold(),
            _web_server: web_server,
        })
    }
}

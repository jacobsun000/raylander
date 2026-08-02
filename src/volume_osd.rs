use std::{cell::Cell, fs, rc::Rc, time::Duration};

use anyhow::{Context, Result, bail};
use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::{
    audio::VolumeState,
    config::{Config, Position, VolumeOsdConfig, theme_path},
};

const DEFAULT_CSS: &str = include_str!("../assets/default.css");

pub struct ThemeManager {
    display: gdk::Display,
    user_provider: Option<gtk::CssProvider>,
}

impl ThemeManager {
    pub fn new(config: &Config) -> Self {
        let display = gdk::Display::default().expect("a Wayland display is required");
        let defaults = gtk::CssProvider::new();
        defaults.load_from_string(DEFAULT_CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &defaults,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut manager = Self {
            display,
            user_provider: None,
        };
        if let Err(error) = manager.reload(config) {
            tracing::warn!(%error, "using embedded theme");
        }
        manager
    }

    pub fn reload(&mut self, config: &Config) -> Result<()> {
        let path = theme_path(config);
        if !path.exists() {
            if let Some(provider) = self.user_provider.take() {
                gtk::style_context_remove_provider_for_display(&self.display, &provider);
            }
            return Ok(());
        }
        let css = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let provider = gtk::CssProvider::new();
        let invalid = Rc::new(Cell::new(false));
        let invalid_signal = Rc::clone(&invalid);
        provider.connect_parsing_error(move |_, section, error| {
            invalid_signal.set(true);
            tracing::error!(section = ?section, %error, "invalid theme CSS");
        });
        provider.load_from_string(&css);
        if invalid.get() {
            bail!("theme {} contains invalid CSS", path.display());
        }
        gtk::style_context_add_provider_for_display(
            &self.display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        if let Some(previous) = self.user_provider.replace(provider) {
            gtk::style_context_remove_provider_for_display(&self.display, &previous);
        }
        Ok(())
    }
}

pub struct VolumeOsd {
    window: gtk::ApplicationWindow,
    revealer: gtk::Revealer,
    icon: gtk::Image,
    label: gtk::Label,
    progress: gtk::ProgressBar,
    generation: Rc<Cell<u64>>,
    config: VolumeOsdConfig,
}

impl VolumeOsd {
    pub fn new(app: &gtk::Application, config: &VolumeOsdConfig) -> Self {
        let icon = gtk::Image::builder()
            .icon_name("audio-volume-medium-symbolic")
            .pixel_size(28)
            .css_classes(["volume-icon"])
            .build();
        let progress = gtk::ProgressBar::builder()
            .hexpand(true)
            .css_classes(["volume-bar"])
            .build();
        let label = gtk::Label::builder()
            .label("0%")
            .width_chars(5)
            .xalign(1.0)
            .css_classes(["volume-percent"])
            .build();
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(16)
            .css_classes(["volume-osd"])
            .build();
        row.append(&icon);
        row.append(&progress);
        row.append(&label);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .reveal_child(false)
            .child(&row)
            .build();
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(320)
            .default_height(72)
            .decorated(false)
            .focusable(false)
            .resizable(false)
            .child(&revealer)
            .css_classes(["raylander-window"])
            .build();
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("raylander-volume-osd"));
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_exclusive_zone(-1);

        let mut osd = Self {
            window,
            revealer,
            icon,
            label,
            progress,
            generation: Rc::new(Cell::new(0)),
            config: config.clone(),
        };
        osd.apply_config(config);
        osd
    }

    pub fn apply_config(&mut self, config: &VolumeOsdConfig) {
        self.config = config.clone();
        self.revealer.set_transition_duration(config.fade_ms);
        self.window.set_anchor(Edge::Top, false);
        self.window.set_anchor(Edge::Bottom, false);
        match config.position {
            Position::TopCenter => {
                self.window.set_anchor(Edge::Top, true);
                self.window.set_margin(Edge::Top, config.margin);
            }
            Position::BottomCenter => {
                self.window.set_anchor(Edge::Bottom, true);
                self.window.set_margin(Edge::Bottom, config.margin);
            }
        }
        self.retarget_monitor();
        if !config.enabled {
            self.window.set_visible(false);
        }
    }

    pub fn retarget_monitor(&self) {
        let display = gtk::prelude::WidgetExt::display(&self.window);
        let monitors = display.monitors();
        let selected = (0..monitors.n_items())
            .filter_map(|index| monitors.item(index))
            .filter_map(|item| item.downcast::<gdk::Monitor>().ok())
            .find(|monitor| monitor.connector().as_deref() == Some(self.config.monitor.as_str()));
        if selected.is_none() {
            tracing::warn!(monitor = %self.config.monitor, "configured monitor unavailable; using compositor choice");
        }
        self.window.set_monitor(selected.as_ref());
    }

    pub fn show(&self, state: VolumeState) {
        if !self.config.enabled {
            return;
        }
        let icon = if state.muted || state.percent == 0 {
            "audio-volume-muted-symbolic"
        } else if state.percent <= 33 {
            "audio-volume-low-symbolic"
        } else if state.percent <= 66 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        };
        self.icon.set_icon_name(Some(icon));
        if state.muted {
            self.label.set_label("Muted");
            self.progress.set_fraction(0.0);
        } else {
            self.label.set_label(&format!("{}%", state.percent));
            self.progress.set_fraction(
                f64::from(state.percent) / f64::from(self.config.limit_percent.max(1)),
            );
        }

        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.revealer.set_reveal_child(true);
        self.window.present();

        let revealer = self.revealer.clone();
        let window = self.window.clone();
        let current = Rc::clone(&self.generation);
        let fade_ms = self.config.fade_ms;
        glib::timeout_add_local_once(Duration::from_millis(self.config.timeout_ms), move || {
            if current.get() != generation {
                return;
            }
            revealer.set_reveal_child(false);
            let current = Rc::clone(&current);
            glib::timeout_add_local_once(Duration::from_millis(u64::from(fade_ms)), move || {
                if current.get() == generation {
                    window.set_visible(false);
                }
            });
        });
    }
}

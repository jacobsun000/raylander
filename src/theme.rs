use std::{cell::Cell, fs, rc::Rc};

use anyhow::{Context, Result, bail};
use gtk::gdk;

use crate::config::{self, ColorScheme, Config};

const ATLAS_LIGHT: &str = include_str!("../assets/atlas-light.css");
const ATLAS_DARK: &str = include_str!("../assets/atlas-dark.css");
const COMPONENTS: &str = include_str!("../assets/default.css");

/// Owns the application-wide Atlas stylesheet and optional user overrides.
pub struct ThemeManager {
    display: gdk::Display,
    atlas_provider: gtk::CssProvider,
    user_provider: Option<gtk::CssProvider>,
    reduce_transparency: bool,
}

impl ThemeManager {
    pub fn new(config: &Config) -> Self {
        let display = gdk::Display::default().expect("a Wayland display is required");
        let atlas_provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &atlas_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let mut manager = Self {
            display,
            atlas_provider,
            user_provider: None,
            reduce_transparency: config.reduce_transparency,
        };
        if let Err(error) = manager.reload(config) {
            tracing::warn!(%error, "using embedded Atlas theme");
            manager.load_atlas(ColorScheme::Dark, config.reduce_transparency);
        }
        manager
    }

    pub fn reload(&mut self, config: &Config) -> Result<()> {
        self.reduce_transparency = config.reduce_transparency;
        self.load_atlas(
            resolve_scheme(config.color_scheme),
            self.reduce_transparency,
        );
        self.reload_user_css(config)
    }

    pub fn preview_scheme(&self, scheme: ColorScheme) {
        self.load_atlas(scheme, self.reduce_transparency);
    }

    fn load_atlas(&self, scheme: ColorScheme, reduce_transparency: bool) {
        let tokens = match scheme {
            ColorScheme::Light => ATLAS_LIGHT,
            ColorScheme::Dark | ColorScheme::System => ATLAS_DARK,
        };
        let solid = if reduce_transparency {
            ".atlas-menu-bar,.atlas-sidebar,.atlas-tile,.atlas-notification,.atlas-menu{background:@atlas_surface;}"
        } else {
            ""
        };
        self.atlas_provider
            .load_from_string(&format!("{tokens}\n{COMPONENTS}\n{solid}"));
    }

    fn reload_user_css(&mut self, config: &Config) -> Result<()> {
        let path = config::theme_path(config);
        if !path.exists() {
            self.remove_user_provider();
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

    fn remove_user_provider(&mut self) {
        if let Some(provider) = self.user_provider.take() {
            gtk::style_context_remove_provider_for_display(&self.display, &provider);
        }
    }
}

fn resolve_scheme(scheme: ColorScheme) -> ColorScheme {
    if scheme != ColorScheme::System {
        return scheme;
    }
    let prefers_dark = gtk::Settings::default()
        .is_some_and(|settings| settings.is_gtk_application_prefer_dark_theme());
    if prefers_dark {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    }
}

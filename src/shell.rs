use std::{
    cell::Cell, collections::HashMap, path::PathBuf, process::Command, rc::Rc, thread,
    time::Duration,
};

use anyhow::{Context, Result};
use gtk::{gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::calendar::{CivilDate, MonthGrid};
use crate::config::{
    BarItem, ComponentId, Config, DesktopWidgetConfig, MenuBarConfig, SidebarSection,
};
use crate::notifications::{ExpireTimeout, Notification, Urgency};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellState {
    pub active_app: String,
    pub media: String,
    pub packages: Option<usize>,
    pub input_method: String,
    pub audio: String,
    pub microphone: String,
    pub network: String,
    pub notification_count: usize,
    pub workspaces: HashMap<String, String>,
    pub system_health: String,
    pub storage: String,
    pub display: String,
    pub power: String,
    pub privacy: String,
    pub bluetooth: String,
    pub tray_icons: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarTarget {
    QuickSettings,
    Playing,
    Calendar,
    Packages,
    Notifications,
}

pub struct ShellUi {
    bars: Vec<gtk::ApplicationWindow>,
    desktop_windows: Vec<gtk::ApplicationWindow>,
    sidebar: gtk::ApplicationWindow,
    sidebar_revealer: gtk::Revealer,
    sidebar_open: Rc<Cell<bool>>,
    sidebar_targets: HashMap<SidebarTarget, gtk::Widget>,
    labels: LabelBindings,
    banner: Banner,
    clock_running: Rc<Cell<bool>>,
}

struct Banner {
    window: gtk::ApplicationWindow,
    revealer: gtk::Revealer,
    title: gtk::Label,
    body: gtk::Label,
    generation: Rc<Cell<u64>>,
}

#[derive(Default)]
struct LabelBindings {
    active_app: Vec<gtk::Label>,
    workspaces: Vec<(String, gtk::Label)>,
    media: Vec<gtk::Label>,
    packages: Vec<gtk::Label>,
    input_method: Vec<gtk::Label>,
    audio: Vec<gtk::Label>,
    network: Vec<gtk::Label>,
    notifications: Vec<gtk::Label>,
    clocks: Vec<gtk::Label>,
    dates: Vec<gtk::Label>,
    system_health: Vec<gtk::Label>,
    storage: Vec<gtk::Label>,
    display: Vec<gtk::Label>,
    power: Vec<gtk::Label>,
    privacy: Vec<gtk::Label>,
    bluetooth: Vec<gtk::Label>,
    tray: Vec<gtk::Box>,
}

impl ShellUi {
    pub fn new(app: &gtk::Application, config: &Config) -> Self {
        let mut labels = LabelBindings::default();
        let sidebar_open = Rc::new(Cell::new(false));
        let sidebar_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(250)
            .reveal_child(false)
            .build();
        let (sidebar, sidebar_targets) = build_sidebar(
            app,
            &config.components.sidebar.sections,
            &sidebar_revealer,
            &mut labels,
        );
        let bars = build_bars(
            app,
            &config.components.menu_bar,
            &sidebar,
            &sidebar_revealer,
            Rc::clone(&sidebar_open),
            &mut labels,
        );
        let desktop_windows = build_desktops(
            app,
            &config.components.desktop.widgets,
            config.components.desktop.enabled,
            &mut labels,
        );
        let banner = build_banner(app);
        let clock_running = Rc::new(Cell::new(true));

        let ui = Self {
            bars,
            desktop_windows,
            sidebar,
            sidebar_revealer,
            sidebar_open,
            sidebar_targets,
            labels,
            banner,
            clock_running,
        };
        ui.start_clock();
        ui
    }

    pub fn update(&self, state: &ShellState) {
        set_all(
            &self.labels.active_app,
            text_or(&state.active_app, "Desktop"),
        );
        for (monitor, label) in &self.labels.workspaces {
            label.set_label(
                state
                    .workspaces
                    .get(monitor)
                    .map(String::as_str)
                    .unwrap_or("Workspace"),
            );
        }
        set_all(&self.labels.media, text_or(&state.media, "Nothing Playing"));
        set_all(
            &self.labels.packages,
            &state
                .packages
                .map(|count| count.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        set_all(
            &self.labels.input_method,
            text_or(&state.input_method, "EN"),
        );
        let audio = match (state.audio.is_empty(), state.microphone.is_empty()) {
            (false, false) => format!("{} · {}", state.audio, state.microphone),
            (false, true) => state.audio.clone(),
            (true, false) => state.microphone.clone(),
            (true, true) => "Audio".into(),
        };
        set_all(&self.labels.audio, &audio);
        set_all(&self.labels.network, text_or(&state.network, "Offline"));
        set_all(
            &self.labels.notifications,
            &state.notification_count.to_string(),
        );
        set_all(
            &self.labels.system_health,
            text_or(&state.system_health, "Ready"),
        );
        set_all(&self.labels.storage, text_or(&state.storage, "Unavailable"));
        set_all(&self.labels.display, text_or(&state.display, "Unavailable"));
        set_all(&self.labels.power, text_or(&state.power, "Desktop"));
        set_all(&self.labels.privacy, text_or(&state.privacy, "Private"));
        set_all(
            &self.labels.bluetooth,
            text_or(&state.bluetooth, "Unavailable"),
        );
        set_tray_icons(&self.labels.tray, &state.tray_icons);
    }

    pub fn show(&self) {
        for window in &self.bars {
            window.present();
        }
        self.show_desktop();
    }

    pub fn show_desktop(&self) {
        for window in &self.desktop_windows {
            window.present();
        }
    }

    pub fn show_sidebar(&self, target: Option<SidebarTarget>) {
        self.sidebar_open.set(true);
        self.sidebar_revealer.set_reveal_child(true);
        self.sidebar.present();
        if let Some(widget) = target.and_then(|target| self.sidebar_targets.get(&target)) {
            let widget = widget.clone();
            glib::idle_add_local_once(move || {
                widget.grab_focus();
            });
        }
    }

    pub fn hide_sidebar(&self) {
        self.sidebar_open.set(false);
        self.sidebar_revealer.set_reveal_child(false);
        let window = self.sidebar.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            window.set_visible(false);
        });
    }

    pub fn toggle_sidebar(&self, target: Option<SidebarTarget>) {
        if self.sidebar_open.get() {
            self.hide_sidebar();
        } else {
            self.show_sidebar(target);
        }
    }

    pub fn render_demo(&self, path: PathBuf, app: gtk::Application) {
        let (window, gallery) = build_demo_window(&app);
        window.present();
        let attempts = Rc::new(Cell::new(0_u8));
        glib::timeout_add_local(Duration::from_millis(50), move || {
            if gallery.width() <= 0 || gallery.height() <= 0 {
                let attempt = attempts.get().saturating_add(1);
                attempts.set(attempt);
                if attempt < 40 {
                    return glib::ControlFlow::Continue;
                }
            }

            match render_widget_to_png(&window, &gallery, &path) {
                Ok(()) => tracing::info!(path = %path.display(), "rendered shell demo image"),
                Err(error) => tracing::error!(
                    path = %path.display(),
                    %error,
                    "failed to render shell demo image"
                ),
            }
            app.quit();
            glib::ControlFlow::Break
        });
    }

    pub fn show_demo(&self, app: &gtk::Application) {
        let (window, _) = build_demo_window(app);
        window.set_title(Some("Raylander Component Gallery"));
        window.present();
    }

    pub fn show_notification(&self, notification: &Notification) {
        self.banner.title.set_label(&notification.summary);
        self.banner.body.set_label(&notification.body);
        let generation = self.banner.generation.get().wrapping_add(1);
        self.banner.generation.set(generation);
        self.banner.revealer.set_reveal_child(true);
        self.banner.window.present();
        let timeout = match notification.timeout {
            ExpireTimeout::Never => return,
            ExpireTimeout::Milliseconds(value) => Duration::from_millis(u64::from(value)),
            ExpireTimeout::Default => Duration::from_millis(match notification.hints.urgency {
                Urgency::Low => 3_000,
                Urgency::Normal => 5_000,
                Urgency::Critical => 8_000,
            }),
        };
        let revealer = self.banner.revealer.clone();
        let window = self.banner.window.clone();
        let current = Rc::clone(&self.banner.generation);
        glib::timeout_add_local_once(timeout, move || {
            if current.get() != generation {
                return;
            }
            revealer.set_reveal_child(false);
            glib::timeout_add_local_once(Duration::from_millis(250), move || {
                if current.get() == generation {
                    window.set_visible(false);
                }
            });
        });
    }

    fn start_clock(&self) {
        update_clock_labels(&self.labels.clocks, &self.labels.dates);
        let clocks = self.labels.clocks.clone();
        let dates = self.labels.dates.clone();
        let running = Rc::clone(&self.clock_running);
        glib::timeout_add_seconds_local(30, move || {
            if !running.get() {
                return glib::ControlFlow::Break;
            }
            update_clock_labels(&clocks, &dates);
            glib::ControlFlow::Continue
        });
    }
}

impl Drop for ShellUi {
    fn drop(&mut self) {
        self.clock_running.set(false);
        for window in &self.bars {
            window.close();
        }
        for window in &self.desktop_windows {
            window.close();
        }
        self.sidebar.close();
        self.banner.window.close();
    }
}

fn build_bars(
    app: &gtk::Application,
    config: &MenuBarConfig,
    sidebar: &gtk::ApplicationWindow,
    revealer: &gtk::Revealer,
    sidebar_open: Rc<Cell<bool>>,
    labels: &mut LabelBindings,
) -> Vec<gtk::ApplicationWindow> {
    if !config.enabled {
        return Vec::new();
    }
    available_monitors()
        .into_iter()
        .filter(|monitor| monitor_selected(monitor, &config.monitors))
        .map(|monitor| {
            let connector = monitor.connector().unwrap_or_default().to_string();
            let left = build_bar_cluster(
                &config.left,
                sidebar,
                revealer,
                Rc::clone(&sidebar_open),
                labels,
                &connector,
            );
            let center = build_bar_cluster(
                &config.center,
                sidebar,
                revealer,
                Rc::clone(&sidebar_open),
                labels,
                &connector,
            );
            let right = build_bar_cluster(
                &config.right,
                sidebar,
                revealer,
                Rc::clone(&sidebar_open),
                labels,
                &connector,
            );
            let content = gtk::CenterBox::builder()
                .start_widget(&left)
                .center_widget(&center)
                .end_widget(&right)
                .css_classes(["atlas-menu-bar"])
                .build();
            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .decorated(false)
                .focusable(false)
                .resizable(false)
                .child(&content)
                .css_classes(["raylander-window"])
                .build();
            window.init_layer_shell();
            window.set_namespace(Some("raylander-menu-bar"));
            window.set_layer(Layer::Top);
            window.set_keyboard_mode(KeyboardMode::None);
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Left, true);
            window.set_anchor(Edge::Right, true);
            window.set_monitor(Some(&monitor));
            window.auto_exclusive_zone_enable();
            window
        })
        .collect()
}

fn build_bar_cluster(
    items: &[BarItem],
    sidebar: &gtk::ApplicationWindow,
    revealer: &gtk::Revealer,
    sidebar_open: Rc<Cell<bool>>,
    labels: &mut LabelBindings,
    connector: &str,
) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["atlas-menu-cluster"])
        .build();
    for item in items {
        let (button, label, content) = menu_item(*item);
        match item {
            BarItem::ActiveApp => labels.active_app.push(label),
            BarItem::Workspaces => labels.workspaces.push((connector.into(), label)),
            BarItem::Playing => labels.media.push(label),
            BarItem::Packages => labels.packages.push(label),
            BarItem::InputMethod => labels.input_method.push(label),
            BarItem::Audio => labels.audio.push(label),
            BarItem::Network => labels.network.push(label),
            BarItem::Notifications => labels.notifications.push(label),
            BarItem::Clock => labels.clocks.push(label),
            BarItem::Tray => labels.tray.push(content),
            _ => {}
        }
        match item {
            BarItem::SystemMenu => {
                attach_system_menu(&button);
            }
            BarItem::InputMethod => {
                button.connect_clicked(|_| spawn_action("fcitx5-remote", &["-t"]));
            }
            BarItem::Workspaces => {
                button.connect_clicked(|_| {
                    spawn_action("hyprctl", &["dispatch", "workspace", "r+1"])
                });
            }
            _ => {}
        }
        if matches!(
            item,
            BarItem::Playing
                | BarItem::Packages
                | BarItem::Audio
                | BarItem::Network
                | BarItem::Notifications
                | BarItem::Clock
        ) {
            let sidebar = sidebar.clone();
            let revealer = revealer.clone();
            let sidebar_open = Rc::clone(&sidebar_open);
            button.connect_clicked(move |_| {
                toggle_sidebar_window(&sidebar, &revealer, &sidebar_open)
            });
        }
        row.append(&button);
    }
    row
}

fn attach_system_menu(button: &gtk::Button) {
    let menu = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-menu"])
        .build();
    let popover = gtk::Popover::builder().child(&menu).autohide(true).build();
    popover.set_parent(button);
    button.connect_unrealize({
        let popover = popover.downgrade();
        move |_| {
            if let Some(popover) = popover.upgrade()
                && popover.parent().is_some()
            {
                popover.unparent();
            }
        }
    });
    for (label, action) in [
        ("Search", SystemMenuAction::Search),
        ("Control Center", SystemMenuAction::ControlCenter),
        ("Lock Screen", SystemMenuAction::Lock),
    ] {
        let item = gtk::Button::builder()
            .label(label)
            .halign(gtk::Align::Fill)
            .css_classes(["atlas-menu-item"])
            .build();
        let popover = popover.clone();
        item.connect_clicked(move |_| {
            match action {
                SystemMenuAction::Search => spawn_action("vicinae", &["toggle"]),
                SystemMenuAction::ControlCenter => spawn_self(&["sidebar", "toggle", "controls"]),
                SystemMenuAction::Lock => spawn_session(crate::power::SessionAction::Lock),
            }
            popover.popdown();
        });
        menu.append(&item);
    }
    let confirmed = Rc::new(Cell::new(false));
    let power = gtk::Button::builder()
        .label("Power Off…")
        .halign(gtk::Align::Fill)
        .css_classes(["atlas-menu-item"])
        .build();
    power.connect_clicked({
        let confirmed = Rc::clone(&confirmed);
        let popover = popover.clone();
        move |button| {
            if confirmed.replace(true) {
                spawn_session(crate::power::SessionAction::PowerOff);
                popover.popdown();
            } else {
                button.set_label("Confirm Power Off");
                button.add_css_class("atlas-destructive");
            }
        }
    });
    menu.append(&power);
    button.connect_clicked(move |_| {
        confirmed.set(false);
        power.set_label("Power Off…");
        power.remove_css_class("atlas-destructive");
        popover.popup();
    });
}

#[derive(Clone, Copy)]
enum SystemMenuAction {
    Search,
    ControlCenter,
    Lock,
}

fn spawn_session(action: crate::power::SessionAction) {
    thread::spawn(move || {
        if let Err(error) = crate::power::PowerBackend.apply_session_action(action, true) {
            tracing::warn!(%error, ?action, "session action failed");
        }
    });
}

fn spawn_action(program: &str, args: &[&str]) {
    let program = program.to_owned();
    let args: Vec<_> = args.iter().map(|value| (*value).to_owned()).collect();
    thread::spawn(move || match Command::new(&program).args(&args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(%status, %program, "desktop action failed"),
        Err(error) => tracing::warn!(%error, %program, "failed to start desktop action"),
    });
}

fn menu_item(item: BarItem) -> (gtk::Button, gtk::Label, gtk::Box) {
    let text = match item {
        BarItem::SystemMenu => "Raylander",
        BarItem::ActiveApp => "Desktop",
        BarItem::Workspaces => "●  ○  ○",
        BarItem::Playing => "Nothing Playing",
        BarItem::Packages => "—",
        BarItem::InputMethod => "EN",
        BarItem::Tray => "",
        BarItem::Audio => "Audio",
        BarItem::Network => "Offline",
        BarItem::Notifications => "0",
        BarItem::Clock => "--:--",
    };
    let label = gtk::Label::new(Some(text));
    if matches!(item, BarItem::SystemMenu | BarItem::ActiveApp) {
        label.add_css_class("atlas-app-name");
    } else if matches!(
        item,
        BarItem::Packages | BarItem::InputMethod | BarItem::Notifications | BarItem::Clock
    ) {
        label.add_css_class("atlas-menu-meta");
    }
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    if let Some(icon) = bar_item_icon(item) {
        content.append(
            &gtk::Image::builder()
                .icon_name(icon)
                .pixel_size(14)
                .css_classes(["atlas-menu-icon"])
                .build(),
        );
    }
    content.append(&label);
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["atlas-menu-item"])
        .build();
    (button, label, content)
}

fn bar_item_icon(item: BarItem) -> Option<&'static str> {
    match item {
        BarItem::SystemMenu => Some("preferences-system-symbolic"),
        BarItem::Playing => Some("media-playback-start-symbolic"),
        BarItem::Packages => Some("software-update-available-symbolic"),
        BarItem::Tray | BarItem::ActiveApp | BarItem::Workspaces | BarItem::Clock => None,
        BarItem::InputMethod => Some("input-keyboard-symbolic"),
        BarItem::Audio => Some("audio-volume-high-symbolic"),
        BarItem::Network => Some("network-wireless-symbolic"),
        BarItem::Notifications => Some("preferences-system-notifications-symbolic"),
    }
}

fn build_demo_gallery() -> gtk::Box {
    let canvas = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-demo-canvas"])
        .build();
    let bar = gtk::CenterBox::builder()
        .start_widget(
            &gtk::Label::builder()
                .label("Raylander     Firefox")
                .xalign(0.0)
                .css_classes(["atlas-app-name"])
                .build(),
        )
        .center_widget(
            &gtk::Label::builder()
                .label("Workspace 10")
                .css_classes(["atlas-menu-meta"])
                .build(),
        )
        .end_widget(
            &gtk::Label::builder()
                .label("EN    12 updates    35% · Mic 100%    Wi-Fi    10:42 PM")
                .xalign(1.0)
                .css_classes(["atlas-menu-meta"])
                .build(),
        )
        .css_classes(["atlas-menu-bar"])
        .build();
    canvas.append(&bar);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["atlas-demo-body"])
        .build();
    let grid = gtk::Grid::builder()
        .css_classes(["atlas-desktop-grid"])
        .build();
    let mut labels = LabelBindings::default();
    for widget in [
        DesktopWidgetConfig::demo(
            ComponentId::Playing,
            crate::config::GridSize::TwoByOne,
            0,
            0,
        ),
        DesktopWidgetConfig::demo(ComponentId::Clock, crate::config::GridSize::TwoByOne, 2, 0),
        DesktopWidgetConfig::demo(
            ComponentId::SystemHealth,
            crate::config::GridSize::TwoByOne,
            0,
            1,
        ),
        DesktopWidgetConfig::demo(
            ComponentId::Calendar,
            crate::config::GridSize::TwoByTwo,
            2,
            1,
        ),
    ] {
        let tile = desktop_tile(&widget, &mut labels);
        grid.attach(
            &tile,
            i32::from(widget.column),
            i32::from(widget.row),
            i32::from(widget.presentation.columns()),
            i32::from(widget.presentation.rows()),
        );
    }
    set_all(&labels.media, "Everything in Its Right Place\nRadiohead");
    set_all(&labels.system_health, "Load 0.42\nMemory 38%");
    update_clock_labels(&labels.clocks, &labels.dates);
    body.append(&grid);

    let sidebar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-sidebar"])
        .build();
    sidebar.append(
        &gtk::Label::builder()
            .label("Control Center")
            .xalign(0.0)
            .css_classes(["atlas-sidebar-header"])
            .build(),
    );
    sidebar.append(&sidebar_section(SidebarSection::QuickSettings).0);
    sidebar.append(&sidebar_section(SidebarSection::Playing).0);
    sidebar.append(&notification_demo());
    body.append(&sidebar);
    canvas.append(&body);
    canvas
}

fn build_demo_window(app: &gtk::Application) -> (gtk::ApplicationWindow, gtk::Box) {
    let gallery = build_demo_gallery();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(1000)
        .default_height(760)
        .decorated(false)
        .resizable(false)
        .child(&gallery)
        .css_classes(["raylander-window"])
        .build();
    (window, gallery)
}

fn build_banner(app: &gtk::Application) -> Banner {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["atlas-headline"])
        .build();
    let body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(42)
        .css_classes(["atlas-body"])
        .build();
    let copy = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    copy.append(&title);
    copy.append(&body);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .css_classes(["atlas-notification"])
        .build();
    content.append(&icon_tile(
        "preferences-system-notifications-symbolic",
        "accent",
    ));
    content.append(&copy);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .transition_duration(250)
        .reveal_child(false)
        .child(&content)
        .build();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .focusable(false)
        .resizable(false)
        .child(&revealer)
        .css_classes(["raylander-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("raylander-notifications"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 40);
    window.set_margin(Edge::Right, 8);
    window.set_exclusive_zone(-1);
    Banner {
        window,
        revealer,
        title,
        body,
        generation: Rc::new(Cell::new(0)),
    }
}

fn notification_demo() -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .css_classes(["atlas-notification"])
        .build();
    card.append(&icon_tile("dialog-information-symbolic", "accent"));
    let copy = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    for (text, class) in [
        ("RAYLANDER  ·  NOW", "atlas-caption"),
        ("Desktop shell is ready", "atlas-headline"),
        (
            "One quiet surface for the things that matter.",
            "atlas-body",
        ),
    ] {
        copy.append(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .wrap(true)
                .css_classes([class])
                .build(),
        );
    }
    card.append(&copy);
    card
}

fn build_sidebar(
    app: &gtk::Application,
    sections: &[SidebarSection],
    revealer: &gtk::Revealer,
    labels: &mut LabelBindings,
) -> (gtk::ApplicationWindow, HashMap<SidebarTarget, gtk::Widget>) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-sidebar"])
        .build();
    content.append(
        &gtk::Label::builder()
            .label("Control Center")
            .xalign(0.0)
            .css_classes(["atlas-sidebar-header"])
            .build(),
    );
    let mut targets = HashMap::new();
    for section in sections {
        if matches!(section, SidebarSection::QuickSettings) {
            let widget = quick_settings();
            widget.set_focusable(true);
            targets.insert(SidebarTarget::QuickSettings, widget.clone().upcast());
            content.append(&widget);
            continue;
        }
        let (widget, body) = sidebar_section(*section);
        widget.set_focusable(true);
        targets.insert(
            sidebar_target_for_section(*section),
            widget.clone().upcast(),
        );
        if let Some(actions) = sidebar_actions(*section) {
            widget.append(&actions);
        }
        match section {
            SidebarSection::Playing => labels.media.push(body),
            SidebarSection::Packages => labels.packages.push(body),
            SidebarSection::Notifications => labels.notifications.push(body),
            SidebarSection::QuickSettings | SidebarSection::Calendar => {}
        }
        content.append(&widget);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .build();
    revealer.set_child(Some(&scroller));
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .focusable(true)
        .resizable(false)
        .child(revealer)
        .css_classes(["raylander-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("raylander-sidebar"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Top, 40);
    window.set_margin(Edge::Right, 8);
    window.set_margin(Edge::Bottom, 8);
    window.set_exclusive_zone(-1);
    (window, targets)
}

fn sidebar_target_for_section(section: SidebarSection) -> SidebarTarget {
    match section {
        SidebarSection::QuickSettings => SidebarTarget::QuickSettings,
        SidebarSection::Playing => SidebarTarget::Playing,
        SidebarSection::Calendar => SidebarTarget::Calendar,
        SidebarSection::Packages => SidebarTarget::Packages,
        SidebarSection::Notifications => SidebarTarget::Notifications,
    }
}

fn sidebar_actions(section: SidebarSection) -> Option<gtk::Box> {
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .halign(gtk::Align::End)
        .build();
    let add = |label: &str, args: &'static [&'static str]| {
        let button = gtk::Button::builder()
            .label(label)
            .css_classes(["atlas-menu-item"])
            .build();
        button.connect_clicked(move |_| spawn_self(args));
        actions.append(&button);
    };
    match section {
        SidebarSection::Playing => {
            add("Previous", &["media", "previous"]);
            add("Play / Pause", &["media", "play-pause"]);
            add("Next", &["media", "next"]);
        }
        SidebarSection::Packages => add("Refresh", &["packages", "refresh"]),
        SidebarSection::Notifications => {
            add("Focus", &["notifications", "dnd", "toggle"]);
            add("Clear", &["notifications", "clear"]);
        }
        SidebarSection::QuickSettings | SidebarSection::Calendar => return None,
    }
    Some(actions)
}

fn quick_settings() -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-section"])
        .build();
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    header.append(&icon_tile("preferences-system-symbolic", "quiet"));
    header.append(
        &gtk::Label::builder()
            .label("QUICK SETTINGS")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["atlas-section-title"])
            .build(),
    );
    container.append(&header);
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_homogeneous(true)
        .build();
    let wifi = quick_button("network-wireless-symbolic", "Wi-Fi");
    wifi.connect_clicked(|_| {
        thread::spawn(|| {
            use crate::network::{NetworkAction, NmcliBackend};
            let backend = NmcliBackend;
            match backend.current() {
                Ok(state) => {
                    if let Err(error) = backend.apply(&NetworkAction::SetWifi(!state.wifi_enabled))
                    {
                        tracing::warn!(%error, "Wi-Fi toggle failed");
                    }
                }
                Err(error) => tracing::warn!(%error, "could not read Wi-Fi state"),
            }
        });
    });
    let audio = quick_button("audio-volume-high-symbolic", "Audio");
    audio.connect_clicked(|_| spawn_self(&["volume", "mute"]));
    let focus = quick_button("notifications-disabled-symbolic", "Focus");
    focus.connect_clicked(|_| spawn_self(&["notifications", "dnd", "toggle"]));
    let input = quick_button("input-keyboard-symbolic", "Input");
    input.connect_clicked(|_| spawn_self(&["input", "toggle"]));
    grid.attach(&wifi, 0, 0, 1, 1);
    grid.attach(&audio, 1, 0, 1, 1);
    grid.attach(&focus, 0, 1, 1, 1);
    grid.attach(&input, 1, 1, 1, 1);
    container.append(&grid);
    container
}

fn quick_button(icon: &str, label: &str) -> gtk::Button {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    content.append(&icon_tile(icon, "accent"));
    content.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["atlas-headline"])
            .build(),
    );
    gtk::Button::builder()
        .child(&content)
        .css_classes(["atlas-control-tile"])
        .build()
}

fn spawn_self(args: &[&str]) {
    let args: Vec<_> = args.iter().map(|value| (*value).to_owned()).collect();
    thread::spawn(move || {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "failed to locate Raylander executable");
                return;
            }
        };
        match Command::new(executable).args(&args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(%status, "Raylander action failed"),
            Err(error) => tracing::warn!(%error, "failed to start Raylander action"),
        }
    });
}

fn sidebar_section(section: SidebarSection) -> (gtk::Box, gtk::Label) {
    let (title, body) = match section {
        SidebarSection::QuickSettings => ("QUICK SETTINGS", "Network  ·  Audio  ·  Focus"),
        SidebarSection::Playing => ("NOW PLAYING", "Nothing is playing"),
        SidebarSection::Calendar => ("CALENDAR", "Local calendar"),
        SidebarSection::Packages => ("UPDATES", "Check package status"),
        SidebarSection::Notifications => ("NOTIFICATIONS", "No new notifications"),
    };
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-section"])
        .build();
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    header.append(&icon_tile(sidebar_icon(section), "quiet"));
    header.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["atlas-section-title"])
            .build(),
    );
    container.append(&header);
    let body = gtk::Label::builder()
        .label(body)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["atlas-body"])
        .build();
    container.append(&body);
    (container, body)
}

fn sidebar_icon(section: SidebarSection) -> &'static str {
    match section {
        SidebarSection::QuickSettings => "preferences-system-symbolic",
        SidebarSection::Playing => "media-playback-start-symbolic",
        SidebarSection::Calendar => "x-office-calendar-symbolic",
        SidebarSection::Packages => "software-update-available-symbolic",
        SidebarSection::Notifications => "preferences-system-notifications-symbolic",
    }
}

fn build_desktops(
    app: &gtk::Application,
    widgets: &[DesktopWidgetConfig],
    enabled: bool,
    labels: &mut LabelBindings,
) -> Vec<gtk::ApplicationWindow> {
    if !enabled {
        return Vec::new();
    }
    let monitors: HashMap<String, gdk::Monitor> = available_monitors()
        .into_iter()
        .filter_map(|monitor| monitor.connector().map(|name| (name.to_string(), monitor)))
        .collect();
    let mut by_monitor: HashMap<&str, Vec<&DesktopWidgetConfig>> = HashMap::new();
    for widget in widgets {
        by_monitor.entry(&widget.monitor).or_default().push(widget);
    }
    by_monitor
        .into_iter()
        .filter_map(|(connector, widgets)| {
            let monitor = monitors.get(connector)?;
            let grid = gtk::Grid::builder()
                .css_classes(["atlas-desktop-grid"])
                .build();
            for widget in widgets {
                let tile = desktop_tile(widget, labels);
                grid.attach(
                    &tile,
                    i32::from(widget.column),
                    i32::from(widget.row),
                    i32::from(widget.presentation.columns()),
                    i32::from(widget.presentation.rows()),
                );
            }
            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .decorated(false)
                .focusable(false)
                .resizable(false)
                .child(&grid)
                .css_classes(["raylander-window"])
                .build();
            window.init_layer_shell();
            window.set_namespace(Some("raylander-desktop-widgets"));
            window.set_layer(Layer::Bottom);
            window.set_keyboard_mode(KeyboardMode::None);
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Right, true);
            window.set_margin(Edge::Top, 48);
            window.set_margin(Edge::Right, 16);
            window.set_exclusive_zone(-1);
            window.set_monitor(Some(monitor));
            Some(window)
        })
        .collect()
}

fn desktop_tile(widget: &DesktopWidgetConfig, labels: &mut LabelBindings) -> gtk::Box {
    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["atlas-tile", widget.presentation.css_class()])
        .build();
    let title = component_title(widget.component);
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    header.append(&icon_tile(component_icon(widget.component), "quiet"));
    header.append(
        &gtk::Label::builder()
            .label(title.to_uppercase())
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["atlas-caption"])
            .build(),
    );
    tile.append(&header);
    if matches!(widget.component, ComponentId::Calendar) {
        let date = gtk::Label::builder()
            .label("Today")
            .xalign(0.0)
            .css_classes(["atlas-headline"])
            .build();
        labels.dates.push(date.clone());
        tile.append(&date);
        if let Some(calendar) = calendar_grid() {
            tile.append(&calendar);
        }
        return tile;
    }
    let value = gtk::Label::builder()
        .label(component_placeholder(widget.component))
        .xalign(0.0)
        .yalign(0.5)
        .vexpand(true)
        .wrap(true)
        .css_classes([if matches!(widget.component, ComponentId::Clock) {
            "atlas-display"
        } else {
            "atlas-headline"
        }])
        .build();
    match widget.component {
        ComponentId::Playing => labels.media.push(value.clone()),
        ComponentId::Clock => labels.clocks.push(value.clone()),
        ComponentId::Calendar => unreachable!("calendar uses its dedicated grid"),
        ComponentId::Notifications => labels.notifications.push(value.clone()),
        ComponentId::Audio => labels.audio.push(value.clone()),
        ComponentId::Network => labels.network.push(value.clone()),
        ComponentId::Packages => labels.packages.push(value.clone()),
        ComponentId::InputMethod => labels.input_method.push(value.clone()),
        ComponentId::SystemHealth => labels.system_health.push(value.clone()),
        ComponentId::Storage => labels.storage.push(value.clone()),
        ComponentId::Display => labels.display.push(value.clone()),
        ComponentId::Power => labels.power.push(value.clone()),
        ComponentId::Privacy => labels.privacy.push(value.clone()),
        ComponentId::Bluetooth => labels.bluetooth.push(value.clone()),
        ComponentId::Weather => {}
    }
    tile.append(&value);
    tile
}

fn icon_tile(icon: &str, tone: &str) -> gtk::Box {
    let tile = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["atlas-icon-tile"])
        .build();
    tile.add_css_class(tone);
    tile.append(&gtk::Image::builder().icon_name(icon).pixel_size(16).build());
    tile
}

fn component_icon(component: ComponentId) -> &'static str {
    match component {
        ComponentId::Playing => "media-playback-start-symbolic",
        ComponentId::Clock => "alarm-symbolic",
        ComponentId::Calendar => "x-office-calendar-symbolic",
        ComponentId::Notifications => "preferences-system-notifications-symbolic",
        ComponentId::Audio => "audio-volume-high-symbolic",
        ComponentId::Network => "network-wireless-symbolic",
        ComponentId::Packages => "software-update-available-symbolic",
        ComponentId::InputMethod => "input-keyboard-symbolic",
        ComponentId::SystemHealth => "computer-symbolic",
        ComponentId::Storage => "drive-harddisk-symbolic",
        ComponentId::Display => "video-display-symbolic",
        ComponentId::Power => "battery-symbolic",
        ComponentId::Privacy => "security-high-symbolic",
        ComponentId::Bluetooth => "bluetooth-symbolic",
        ComponentId::Weather => "weather-clear-symbolic",
    }
}

fn calendar_grid() -> Option<gtk::Grid> {
    let now = glib::DateTime::now_local().ok()?;
    let today = CivilDate {
        year: now.year(),
        month: now.month() as u8,
        day: now.day_of_month() as u8,
    };
    let month = MonthGrid::new(today.year, today.month, today)?;
    let grid = gtk::Grid::builder()
        .column_homogeneous(true)
        .row_homogeneous(true)
        .vexpand(true)
        .build();
    for (column, name) in ["M", "T", "W", "T", "F", "S", "S"].into_iter().enumerate() {
        grid.attach(
            &gtk::Label::builder()
                .label(name)
                .css_classes(["atlas-caption"])
                .build(),
            column as i32,
            0,
            1,
            1,
        );
    }
    for (index, day) in month.days.iter().enumerate() {
        let label = gtk::Label::builder()
            .label(day.date.day.to_string())
            .css_classes(["atlas-calendar-day"])
            .build();
        if day.is_today {
            label.add_css_class("today");
        }
        if !day.in_current_month {
            label.set_opacity(0.35);
        }
        grid.attach(&label, (index % 7) as i32, (index / 7 + 1) as i32, 1, 1);
    }
    Some(grid)
}

fn component_title(component: ComponentId) -> &'static str {
    match component {
        ComponentId::Playing => "Now Playing",
        ComponentId::Clock => "Time",
        ComponentId::Calendar => "Calendar",
        ComponentId::Notifications => "Notifications",
        ComponentId::Audio => "Audio",
        ComponentId::Network => "Network",
        ComponentId::Packages => "Updates",
        ComponentId::InputMethod => "Input Method",
        ComponentId::SystemHealth => "System",
        ComponentId::Storage => "Storage",
        ComponentId::Display => "Display",
        ComponentId::Power => "Power",
        ComponentId::Privacy => "Privacy",
        ComponentId::Bluetooth => "Bluetooth",
        ComponentId::Weather => "Weather",
    }
}

fn component_placeholder(component: ComponentId) -> &'static str {
    match component {
        ComponentId::Playing => "Nothing Playing",
        ComponentId::Clock => "--:--",
        ComponentId::Calendar => "Today",
        ComponentId::Notifications => "All caught up",
        ComponentId::Audio => "Audio",
        ComponentId::Network => "Offline",
        ComponentId::Packages => "—",
        ComponentId::InputMethod => "EN",
        ComponentId::SystemHealth => "Ready",
        ComponentId::Storage | ComponentId::Display => "Unavailable",
        ComponentId::Power => "Desktop",
        ComponentId::Privacy => "Private",
        ComponentId::Bluetooth | ComponentId::Weather => "Unavailable",
    }
}

fn toggle_sidebar_window(
    sidebar: &gtk::ApplicationWindow,
    revealer: &gtk::Revealer,
    open: &Cell<bool>,
) {
    let next = !open.get();
    open.set(next);
    revealer.set_reveal_child(next);
    if next {
        sidebar.present();
    } else {
        let sidebar = sidebar.clone();
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            sidebar.set_visible(false);
        });
    }
}

fn update_clock_labels(clocks: &[gtk::Label], dates: &[gtk::Label]) {
    let Ok(now) = glib::DateTime::now_local() else {
        return;
    };
    if let Ok(time) = now.format("%-I:%M %p") {
        set_all(clocks, &time);
    }
    if let Ok(date) = now.format("%A\n%B %-d") {
        set_all(dates, &date);
    }
}

fn set_all(labels: &[gtk::Label], value: &str) {
    for label in labels {
        label.set_label(value);
    }
}

fn set_tray_icons(containers: &[gtk::Box], icons: &[String]) {
    for container in containers {
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        for icon in icons.iter().take(8) {
            container.append(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .pixel_size(16)
                    .tooltip_text(icon)
                    .css_classes(["atlas-tray-icon"])
                    .build(),
            );
        }
    }
}

fn text_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn available_monitors() -> Vec<gdk::Monitor> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|index| monitors.item(index))
        .filter_map(|item| item.downcast::<gdk::Monitor>().ok())
        .collect()
}

fn monitor_selected(monitor: &gdk::Monitor, selectors: &[String]) -> bool {
    selectors.iter().any(|selector| {
        selector == "*" || monitor.connector().as_deref() == Some(selector.as_str())
    })
}

fn render_widget_to_png(
    window: &gtk::ApplicationWindow,
    widget: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
) -> Result<()> {
    let width = widget.width();
    let height = widget.height();
    anyhow::ensure!(
        width > 0 && height > 0,
        "demo gallery has not been laid out"
    );
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot
        .to_node()
        .context("demo gallery snapshot was empty")?;
    let renderer = window.renderer().context("GTK renderer is unavailable")?;
    let texture = renderer.render_texture(&node, Some(&node.bounds()));
    texture
        .save_to_png(path)
        .with_context(|| format!("failed to save {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_uses_fallback() {
        assert_eq!(text_or("", "fallback"), "fallback");
        assert_eq!(text_or("value", "fallback"), "value");
    }

    #[test]
    fn component_titles_are_user_facing() {
        assert_eq!(component_title(ComponentId::Playing), "Now Playing");
        assert_eq!(component_title(ComponentId::InputMethod), "Input Method");
    }
}

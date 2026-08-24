use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use axum::{Router, http::header, response::IntoResponse, routing::get};
use chrono::{Datelike, Local, Timelike};

use crate::components::GridSize;

use self::backend::{CalendarEvent, GoogleCalendarBackend};
use super::dock_component;

pub(crate) mod backend;

dock_component!(
    CalendarEvents,
    "calendar-events",
    GridSize::ONE_BY_ONE,
    routes()
);

fn routes() -> Router {
    Router::new().route("/todo.svg", get(todo_icon))
}

async fn todo_icon() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml")],
        include_str!("../../../assets/web/todo.svg"),
    )
}

static URLS: OnceLock<Vec<String>> = OnceLock::new();
static EVENTS: OnceLock<Mutex<Vec<CalendarEvent>>> = OnceLock::new();

pub(crate) struct CalendarEventsConfig {
    pub urls: Vec<String>,
}

pub(crate) fn configure(config: CalendarEventsConfig) {
    let _ = URLS.set(config.urls);
}

pub(crate) fn observe(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-calendar-events".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                let urls = URLS.get().cloned().unwrap_or_default();
                if let Ok(events) = GoogleCalendarBackend.events(&urls) {
                    let mut state = EVENTS
                        .get_or_init(|| Mutex::new(Vec::new()))
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if *state != events {
                        *state = events;
                        drop(state);
                        on_change();
                    }
                }
                for _ in 0..300 {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        })
        .expect("failed to start calendar events observer")
}

pub(crate) fn update_fragment() -> String {
    let events = EVENTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let today = Local::now().date_naive();
    let upcoming = events
        .iter()
        .filter(|event| event.start.date_naive() >= today)
        .collect::<Vec<_>>();
    let compact = upcoming
        .iter()
        .filter(|event| event.start.date_naive() <= today.succ_opt().unwrap_or(today))
        .map(|event| format!(r#"<div class="calendar-event-mini"><i></i><span><b>{}</b><small>{}</small></span></div>"#, escape(&event.title), event_time(event)))
        .collect::<String>();
    let details = upcoming
        .iter()
        .map(|event| {
            let action = if event.url.is_empty() {
                String::new()
            } else {
                format!(
                    r#" onclick="window.open('{}','_blank')" role="link""#,
                    escape(&event.url)
                )
            };
            format!(r#"<article class="calendar-event-detail"{action}><time><b>{}</b><small>{}</small></time><div><strong>{}</strong><span>{}</span></div></article>"#, event.start.format("%a"), event_time(event), escape(&event.title), escape(&event.location))
        })
        .collect::<String>();
    let empty = if compact.is_empty() {
        r#"<div class="calendar-empty"><img src="/components/calendar-events/todo.svg" alt="No upcoming events"><span>No upcoming events</span></div>"#
    } else {
        ""
    };
    let empty_class = if compact.is_empty() { " empty" } else { "" };
    format!(
        r#"<section id="calendar-events-card" class="card calendar-events-card{empty_class}" onclick="if(this.dataset.scrolled){{delete this.dataset.scrolled}}else{{document.querySelector('#calendar-events-dialog').showModal()}}" role="button" hx-swap-oob="outerHTML"><div onscroll="this.parentElement.dataset.scrolled='1'">{compact}{empty}</div></section><div id="calendar-events-panel" class="calendar-events-panel" hx-swap-oob="outerHTML"><header><span><b>{}</b><small>{}</small></span><h2>Upcoming</h2></header><main class="{empty_class}">{details}{empty}</main></div>"#,
        Local::now().day(),
        Local::now().format("%b")
    )
}

fn event_time(event: &CalendarEvent) -> String {
    if event.all_day {
        "All day".into()
    } else {
        let hour = event.start.hour();
        let suffix = if hour < 12 { "AM" } else { "PM" };
        format!(
            "{}:{:02} {suffix}",
            if hour.is_multiple_of(12) {
                12
            } else {
                hour % 12
            },
            event.start.minute()
        )
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

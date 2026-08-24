use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::components::GridSize;

use self::backend::SystemStatusBackend;
use super::dock_component;

pub(crate) mod backend;

dock_component!(SystemStatus, "system-status", GridSize::ONE_BY_ONE);

#[derive(Default)]
struct Cache {
    rendered_at: Option<Instant>,
    html: String,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    rendered_at: None,
    html: String::new(),
});

pub(crate) fn observe(
    shutdown: Arc<AtomicBool>,
    on_change: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-system-status".into())
        .spawn(move || {
            let mut previous = String::new();
            while !shutdown.load(Ordering::Relaxed) {
                invalidate();
                let current = update_fragment();
                if !current.is_empty() && current != previous {
                    previous = current;
                    on_change();
                }
                thread::sleep(Duration::from_secs(3));
            }
        })
        .expect("failed to start system status observer")
}

fn invalidate() {
    CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .rendered_at = None;
}

fn metric(html: &mut String, id: &str, percent: u8) {
    html.push_str(&format!(
        r#"<span id="{id}" class="metric-value" hx-swap-oob="outerHTML">{percent}%</span><i id="{id}-bar" hx-swap-oob="outerHTML" style="width:{percent}%"></i>"#,
    ));
}

pub(crate) fn update_fragment() -> String {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if cache
        .rendered_at
        .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    {
        return cache.html.clone();
    }
    let Ok(status) = SystemStatusBackend.current() else {
        return cache.html.clone();
    };
    let mut html = String::new();
    metric(&mut html, "cpu", status.cpu_percent);
    metric(&mut html, "memory", status.memory_percent);
    metric(&mut html, "disk", status.disk_percent);
    if let Some(gpu) = status.gpu_percent {
        metric(&mut html, "gpu", gpu);
    }
    cache.html = html;
    cache.rendered_at = Some(Instant::now());
    cache.html.clone()
}

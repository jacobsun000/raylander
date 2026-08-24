use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::components::GridSize;

use self::backend::PackageBackend;
use super::dock_component;

pub(crate) mod backend;

dock_component!(Packages, "packages", GridSize::ONE_BY_ONE);

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
        .name("raylander-packages".into())
        .spawn(move || {
            let mut previous = String::new();
            while !shutdown.load(Ordering::Relaxed) {
                invalidate();
                let current = update_fragment();
                if !current.is_empty() && current != previous {
                    previous = current;
                    on_change();
                }
                for _ in 0..1_800 {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        })
        .expect("failed to start packages observer")
}

fn invalidate() {
    CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .rendered_at = None;
}

pub(crate) fn update_fragment() -> String {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if cache
        .rendered_at
        .is_some_and(|at| at.elapsed() < Duration::from_secs(30 * 60))
    {
        return cache.html.clone();
    }
    let Ok(packages) = PackageBackend.current() else {
        return cache.html.clone();
    };
    cache.html = format!(
        r#"<p id="packages-total" hx-swap-oob="innerHTML">{}</p><span id="packages-pacman" hx-swap-oob="innerHTML">◆ {}</span><span id="packages-paru" hx-swap-oob="innerHTML">▲ {}</span>"#,
        packages.total_unique(),
        packages.official.len(),
        packages.aur.len(),
    );
    cache.rendered_at = Some(Instant::now());
    cache.html.clone()
}

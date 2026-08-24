use std::{
    process::{Child, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

pub struct AwakeBackend;

impl AwakeBackend {
    pub fn set(&self, active: bool) -> Result<u64> {
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let mut process = PROCESS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stop(&mut process);
        if active {
            *process = Some(
                Command::new("systemd-inhibit")
                    .args([
                        "--what=idle",
                        "--mode=block",
                        "--who=Raylander",
                        "--why=30-minute awake timer",
                        "sleep",
                        "1800",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("failed to start systemd idle inhibitor")?,
            );
        }
        Ok(generation)
    }

    pub fn expire(&self, generation: u64) {
        if GENERATION.load(Ordering::Relaxed) == generation {
            let mut process = PROCESS
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            stop(&mut process);
        }
    }
}

fn stop(process: &mut Option<Child>) {
    if let Some(mut child) = process.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

static GENERATION: AtomicU64 = AtomicU64::new(0);
pub struct AwakeBackend;
impl AwakeBackend {
    pub fn set(&self, active: bool) -> Result<u64> {
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let flags = if active {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        let previous = unsafe { SetThreadExecutionState(flags) };
        anyhow::ensure!(
            previous.0 != 0,
            "SetThreadExecutionState failed: {}",
            windows::core::Error::from_thread()
        );
        Ok(generation)
    }
    pub fn expire(&self, generation: u64) {
        if GENERATION.load(Ordering::Relaxed) == generation {
            let _ = unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

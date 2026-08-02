use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::cli::VolumeCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeState {
    pub percent: u16,
    pub muted: bool,
}

pub struct WpctlBackend;

impl WpctlBackend {
    pub fn apply(&self, command: VolumeCommand, step: u16, limit: u16) -> Result<VolumeState> {
        match command {
            VolumeCommand::Up => self.run_set(&[
                "set-volume",
                "-l",
                &format!("{:.2}", f64::from(limit) / 100.0),
                "@DEFAULT_AUDIO_SINK@",
                &format!("{step}%+"),
            ])?,
            VolumeCommand::Down => {
                self.run_set(&["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{step}%-")])?
            }
            VolumeCommand::Mute => self.run_set(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])?,
            VolumeCommand::Show => {}
        }
        self.current()
    }

    pub fn current(&self) -> Result<VolumeState> {
        let output = Command::new("wpctl")
            .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
            .output()
            .context("failed to execute wpctl get-volume")?;
        if !output.status.success() {
            bail!("wpctl get-volume exited with {}", output.status);
        }
        let stdout = String::from_utf8(output.stdout).context("wpctl returned non-UTF-8 output")?;
        parse_wpctl_volume(&stdout)
    }

    fn run_set(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("wpctl")
            .args(args)
            .status()
            .with_context(|| format!("failed to execute wpctl {}", args.join(" ")))?;
        if !status.success() {
            bail!("wpctl {} exited with {status}", args.join(" "));
        }
        Ok(())
    }
}

pub fn parse_wpctl_volume(output: &str) -> Result<VolumeState> {
    let value = output
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
        .context("wpctl output did not contain a volume value")?;
    if !value.is_finite() || value < 0.0 {
        bail!("wpctl returned an invalid volume value");
    }
    Ok(VolumeState {
        percent: (value * 100.0).round().clamp(0.0, f64::from(u16::MAX)) as u16,
        muted: output.contains("[MUTED]"),
    })
}

pub fn is_relevant_subscription_event(line: &str) -> bool {
    let changed = line.contains("Event 'change'") || line.contains("Event 'new'");
    changed && (line.contains(" on sink #") || line.contains(" on server"))
}

pub struct AudioObserver {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioObserver {
    pub fn start() -> (Self, Receiver<()>) {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_child = Arc::clone(&child);

        let handle = thread::spawn(move || {
            let delays = [1_u64, 2, 5, 10, 30];
            let mut attempts = 0_usize;
            while !thread_stop.load(Ordering::Relaxed) {
                let spawned = Command::new("pactl")
                    .arg("subscribe")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn();
                let mut process = match spawned {
                    Ok(process) => process,
                    Err(error) => {
                        tracing::warn!(%error, "failed to start pactl observer");
                        sleep_interruptibly(&thread_stop, delays[attempts.min(delays.len() - 1)]);
                        attempts += 1;
                        continue;
                    }
                };
                let stdout = process.stdout.take();
                *thread_child.lock().expect("observer child mutex poisoned") = Some(process);
                if let Some(stdout) = stdout {
                    for line in BufReader::new(stdout).lines() {
                        if thread_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match line {
                            Ok(line) if is_relevant_subscription_event(&line) => {
                                if sender.send(()).is_err() {
                                    return;
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                tracing::warn!(%error, "failed reading pactl observer output");
                                break;
                            }
                        }
                    }
                }
                if let Some(mut process) = thread_child
                    .lock()
                    .expect("observer child mutex poisoned")
                    .take()
                {
                    let _ = process.wait();
                }
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                tracing::warn!("pactl observer exited; restarting");
                sleep_interruptibly(&thread_stop, delays[attempts.min(delays.len() - 1)]);
                attempts += 1;
            }
        });

        (
            Self {
                stop,
                child,
                thread: Some(handle),
            },
            receiver,
        )
    }
}

impl Drop for AudioObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(process) = self
            .child
            .lock()
            .expect("observer child mutex poisoned")
            .as_mut()
        {
            let _ = process.kill();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn sleep_interruptibly(stop: &AtomicBool, seconds: u64) {
    for _ in 0..seconds * 10 {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normal_and_muted_volume() {
        assert_eq!(
            parse_wpctl_volume("Volume: 0.36\n").unwrap(),
            VolumeState {
                percent: 36,
                muted: false
            }
        );
        assert_eq!(
            parse_wpctl_volume("Volume: 1.25 [MUTED]\n").unwrap(),
            VolumeState {
                percent: 125,
                muted: true
            }
        );
    }

    #[test]
    fn filters_subscription_events() {
        assert!(is_relevant_subscription_event("Event 'change' on sink #74"));
        assert!(is_relevant_subscription_event("Event 'change' on server"));
        assert!(!is_relevant_subscription_event("Event 'new' on client #12"));
        assert!(!is_relevant_subscription_event(
            "Event 'change' on sink-input #75"
        ));
        assert!(!is_relevant_subscription_event(
            "Event 'remove' on sink #74"
        ));
    }
}

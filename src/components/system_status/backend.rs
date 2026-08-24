use std::{fs, process::Command, thread, time::Duration};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemStatus {
    pub cpu_percent: u8,
    pub memory_percent: u8,
    pub disk_percent: u8,
    pub gpu_percent: Option<u8>,
}

pub struct SystemStatusBackend;

impl SystemStatusBackend {
    pub fn current(&self) -> Result<SystemStatus> {
        let first = read_cpu_times()?;
        thread::sleep(Duration::from_millis(120));
        let second = read_cpu_times()?;
        let total_delta = second.0.saturating_sub(first.0);
        let idle_delta = second.1.saturating_sub(first.1);
        let cpu_percent = if total_delta == 0 {
            0
        } else {
            ((total_delta - idle_delta) * 100 / total_delta).min(100) as u8
        };

        let memory = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
        let total = meminfo_value(&memory, "MemTotal:")?;
        let available = meminfo_value(&memory, "MemAvailable:")?;
        let memory_percent = if total == 0 {
            0
        } else {
            ((total - available) * 100 / total).min(100) as u8
        };

        Ok(SystemStatus {
            cpu_percent,
            memory_percent,
            disk_percent: disk_usage()?,
            gpu_percent: gpu_usage(),
        })
    }
}

fn read_cpu_times() -> Result<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").context("failed to read /proc/stat")?;
    let values: Vec<u64> = stat
        .lines()
        .next()
        .context("/proc/stat was empty")?
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .context("/proc/stat contained invalid CPU counters")?;
    if values.len() < 4 {
        bail!("/proc/stat omitted CPU counters");
    }
    let total = values.iter().sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Ok((total, idle))
}

fn meminfo_value(input: &str, key: &str) -> Result<u64> {
    input
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|value| value.split_whitespace().next())
        .context(format!("meminfo omitted {key}"))?
        .parse()
        .context("invalid meminfo value")
}

fn disk_usage() -> Result<u8> {
    let output = Command::new("df")
        .args(["-Pk", "/"])
        .output()
        .context("failed to execute df")?;
    if !output.status.success() {
        bail!("df exited with {}", output.status);
    }
    let text = String::from_utf8(output.stdout).context("df returned non-UTF-8 output")?;
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(4))
        .and_then(|value| value.trim_end_matches('%').parse().ok())
        .context("invalid df output")
}

fn gpu_usage() -> Option<u8> {
    for card in fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let path = card.path().join("device/gpu_busy_percent");
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(percent) = value.trim().parse::<u8>()
        {
            return Some(percent.min(100));
        }
    }
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse::<u8>()
        .ok()
        .map(|v| v.min(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo_values() {
        assert_eq!(
            meminfo_value("MemTotal: 16000 kB\n", "MemTotal:").unwrap(),
            16000
        );
        assert!(meminfo_value("", "MemTotal:").is_err());
    }
}

use std::{thread, time::Duration};

use anyhow::Result;

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
        let first = platform::read_cpu_times()?;
        thread::sleep(Duration::from_millis(120));
        let second = platform::read_cpu_times()?;

        Ok(SystemStatus {
            cpu_percent: cpu_usage(first, second),
            memory_percent: platform::memory_usage()?,
            disk_percent: platform::disk_usage()?,
            gpu_percent: platform::gpu_usage(),
        })
    }
}

fn usage_percent(total: u64, available: u64) -> u8 {
    total
        .saturating_sub(available)
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8
}

fn cpu_usage(first: (u64, u64), second: (u64, u64)) -> u8 {
    usage_percent(
        second.0.saturating_sub(first.0),
        second.1.saturating_sub(first.1),
    )
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{fs, process::Command};

    use anyhow::{Context, Result, bail};

    use super::usage_percent;

    pub(super) fn read_cpu_times() -> Result<(u64, u64)> {
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

    pub(super) fn memory_usage() -> Result<u8> {
        let memory = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
        let total = meminfo_value(&memory, "MemTotal:")?;
        let available = meminfo_value(&memory, "MemAvailable:")?;
        Ok(usage_percent(total, available))
    }

    fn meminfo_value(input: &str, key: &str) -> Result<u64> {
        input
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|value| value.split_whitespace().next())
            .with_context(|| format!("meminfo omitted {key}"))?
            .parse()
            .context("invalid meminfo value")
    }

    pub(super) fn disk_usage() -> Result<u8> {
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

    pub(super) fn gpu_usage() -> Option<u8> {
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
            .map(|value| value.min(100))
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
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{env, mem::size_of, os::windows::ffi::OsStrExt};

    use anyhow::{Context, Result};

    use super::usage_percent;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    impl FileTime {
        fn ticks(&self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }

    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_available: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    fn last_error(operation: &str) -> anyhow::Error {
        // SAFETY: GetLastError has no preconditions and is called immediately after failure.
        let code = unsafe { GetLastError() };
        anyhow::anyhow!("{operation} failed with Windows error {code}")
    }

    pub(super) fn read_cpu_times() -> Result<(u64, u64)> {
        let (mut idle, mut kernel, mut user) = Default::default();
        // SAFETY: all pointers refer to valid, writable FILETIME-compatible values.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return Err(last_error("GetSystemTimes"));
        }
        Ok((kernel.ticks().saturating_add(user.ticks()), idle.ticks()))
    }

    pub(super) fn memory_usage() -> Result<u8> {
        let mut status = MemoryStatusEx {
            length: size_of::<MemoryStatusEx>() as u32,
            ..Default::default()
        };
        // SAFETY: status is correctly sized and initialized as required by the API.
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return Err(last_error("GlobalMemoryStatusEx"));
        }
        Ok(usage_percent(status.total_phys, status.avail_phys))
    }

    pub(super) fn disk_usage() -> Result<u8> {
        let drive = env::var_os("SystemDrive").context("SystemDrive is not set")?;
        let mut path: Vec<u16> = drive.encode_wide().collect();
        path.extend(['\\' as u16, 0]);
        let (mut free_available, mut total, mut total_free) = (0, 0, 0);
        // SAFETY: path is NUL-terminated and output pointers are valid writable u64 values.
        if unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut free_available,
                &mut total,
                &mut total_free,
            )
        } == 0
        {
            return Err(last_error("GetDiskFreeSpaceExW"));
        }
        Ok(usage_percent(total, total_free))
    }

    pub(super) fn gpu_usage() -> Option<u8> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_usage_and_handles_invalid_counters() {
        assert_eq!(usage_percent(1_000, 250), 75);
        assert_eq!(usage_percent(0, 0), 0);
        assert_eq!(usage_percent(100, 200), 0);
        assert_eq!(cpu_usage((100, 40), (200, 65)), 75);
        assert_eq!(cpu_usage((200, 100), (100, 50)), 0);
    }
}

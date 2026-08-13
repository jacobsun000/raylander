use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rustfft::{FftPlanner, num_complex::Complex};

const FFT_SIZE: usize = 2048;
const BANDS: usize = 16;

pub fn observe() -> mpsc::Receiver<[u8; BANDS]> {
    let (sender, receiver) = mpsc::sync_channel(2);
    thread::Builder::new()
        .name("raylander-spectrum".into())
        .spawn(move || capture(sender))
        .expect("start spectrum observer");
    receiver
}

fn capture(sender: mpsc::SyncSender<[u8; BANDS]>) {
    loop {
        let Some(device) = default_monitor() else {
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        let Ok(mut child) = Command::new("parec")
            .args([
                "--device",
                &device,
                "--format=float32le",
                "--rate=48000",
                "--channels=1",
                "--latency-msec=40",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        let Some(mut stdout) = child.stdout.take() else {
            continue;
        };
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let mut bytes = vec![0_u8; FFT_SIZE * 4];
        let mut smooth = [0_f32; BANDS];
        let mut last_emit = Instant::now();
        while stdout.read_exact(&mut bytes).is_ok() {
            let mut samples = bytes
                .chunks_exact(4)
                .enumerate()
                .map(|(index, bytes)| {
                    let sample = f32::from_le_bytes(bytes.try_into().unwrap_or_default());
                    let window =
                        0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / FFT_SIZE as f32).cos();
                    Complex::new(sample * window, 0.0)
                })
                .collect::<Vec<_>>();
            fft.process(&mut samples);
            let bands = spectrum(&samples, &mut smooth);
            if last_emit.elapsed() >= Duration::from_millis(45) {
                let _ = sender.try_send(bands);
                last_emit = Instant::now();
            }
        }
        let _ = child.kill();
        thread::sleep(Duration::from_secs(1));
    }
}

fn default_monitor() -> Option<String> {
    let output = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| format!("{}.monitor", String::from_utf8_lossy(&output.stdout).trim()))
}

fn spectrum(samples: &[Complex<f32>], smooth: &mut [f32; BANDS]) -> [u8; BANDS] {
    let mut result = [0_u8; BANDS];
    let min_bin = 3_f32;
    let max_bin = (FFT_SIZE / 2) as f32;
    for band in 0..BANDS {
        let start = (min_bin * (max_bin / min_bin).powf(band as f32 / BANDS as f32)) as usize;
        let end = (min_bin * (max_bin / min_bin).powf((band + 1) as f32 / BANDS as f32)) as usize;
        let peak = samples[start..end.max(start + 1)]
            .iter()
            .map(|value| value.norm())
            .fold(0_f32, f32::max);
        let level = ((peak / FFT_SIZE as f32).max(1e-6).log10() + 5.0) / 4.5;
        let level = level.clamp(0.0, 1.0);
        smooth[band] = if level > smooth[band] {
            smooth[band] * 0.35 + level * 0.65
        } else {
            smooth[band] * 0.82 + level * 0.18
        };
        result[band] = (smooth[band] * 100.0) as u8;
    }
    result
}

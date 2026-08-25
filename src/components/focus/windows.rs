use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use windows::{
    Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        },
    },
    core::PWSTR,
};

use crate::components::{Component, ComponentId, ComponentView, GridSize};

pub struct Focus;

impl Component for Focus {
    fn id(&self) -> ComponentId {
        ComponentId::new("focus")
    }

    fn size(&self) -> GridSize {
        GridSize::ONE_BY_ONE
    }

    fn render(&self) -> Result<ComponentView> {
        Ok(ComponentView {
            body: windows_view(),
            overlays: String::new(),
        })
    }
}

pub(crate) fn observe(
    shutdown: Arc<AtomicBool>,
    on_foreground: impl Fn() + Send + 'static,
    _on_monitor: impl Fn() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("raylander-windows-focus".into())
        .spawn(move || {
            let mut previous = foreground_app();
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
                let current = foreground_app();
                if current != previous {
                    previous = current;
                    on_foreground();
                }
            }
        })
        .expect("failed to start Windows focus observer")
}

pub(crate) fn oob_fragment() -> String {
    update_fragment().map_or_else(String::new, |fragment| {
        format!(r#"<span id="monitors" hx-swap-oob="innerHTML">{fragment}</span>"#)
    })
}

pub(crate) fn update_fragment() -> Option<String> {
    let app = foreground_app().unwrap_or_else(|| "Desktop".to_owned());
    let app = escape(&app);
    Some(format!(r#"<p class="focus-title" title="{app}">{app}</p>"#))
}

fn windows_view() -> String {
    let content = update_fragment()
        .unwrap_or_else(|| r#"<p class="focus-title" title="Desktop">Desktop</p>"#.to_owned());
    format!(
        r#"<section class="card" id="focus-card"><div class="monitor-layout" id="monitors">{content}</div></section>"#
    )
}

fn foreground_app() -> Option<String> {
    // SAFETY: Handles and buffers are checked before use, and the process handle
    // is closed on every successful OpenProcess path.
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return None;
        }

        let mut process_id = 0;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        if process_id != 0
            && let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)
        {
            let mut image = vec![0u16; 32_768];
            let mut length = image.len() as u32;
            let queried = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_FORMAT(0),
                PWSTR(image.as_mut_ptr()),
                &mut length,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if queried {
                let path = String::from_utf16_lossy(&image[..length as usize]);
                if let Some(name) = Path::new(&path).file_stem().and_then(|name| name.to_str())
                    && !name.is_empty()
                {
                    return Some(name.to_owned());
                }
            }
        }

        let length = GetWindowTextLengthW(window);
        if length > 0 {
            let mut title = vec![0u16; length as usize + 1];
            let copied = GetWindowTextW(window, &mut title);
            if copied > 0 {
                return Some(String::from_utf16_lossy(&title[..copied as usize]));
            }
        }
        None
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

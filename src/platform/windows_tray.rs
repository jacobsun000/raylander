use std::{
    fs,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::OnceLock,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE,
            HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW, ShellExecuteW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW,
                IDI_APPLICATION, LoadIconW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MF_SEPARATOR,
                MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
                RegisterWindowMessageW, SW_SHOWNORMAL, SetForegroundWindow, TPM_BOTTOMALIGN,
                TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONDBLCLK,
                WM_NULL, WM_RBUTTONUP, WNDCLASSW,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{app, config};

const TRAY_CALLBACK: u32 = WM_APP + 1;
const OPEN_DOCK_MESSAGE: u32 = WM_APP + 2;
const TRAY_ICON_ID: u32 = 1;
const MENU_OPEN_DOCK: u16 = 1_001;
const MENU_OPEN_CONFIG: u16 = 1_002;
const MENU_REINSTALL_HOOKS: u16 = 1_003;
const MENU_EXIT: u16 = 1_004;

static DOCK_URL: OnceLock<String> = OnceLock::new();
static TASKBAR_CREATED_MESSAGE: OnceLock<u32> = OnceLock::new();

pub fn run_tray() -> Result<()> {
    app::init_tracing();

    let instance = SingleInstance::acquire()?;
    if instance.is_none() {
        activate_existing_instance().context("failed to open the running Raylander dock")?;
        return Ok(());
    }
    let _instance = instance.expect("single-instance mutex was acquired");

    let (daemon, dock_url) = app::start_daemon(true)?;
    let _ = DOCK_URL.set(dock_url);

    let window = MessageWindow::create()?;
    register_taskbar_created_message()?;
    let tray_icon = TrayIcon::add(window.handle())?;

    let message_result = run_message_loop();
    drop(tray_icon);
    drop(window);
    // Dropping the server handle sends its shutdown signal and joins its thread.
    drop(daemon);
    message_result
}

pub fn show_startup_error(message: &str) {
    show_message("Raylander could not start", message, MB_ICONERROR);
}

struct SingleInstance(HANDLE);

impl SingleInstance {
    fn acquire() -> Result<Option<Self>> {
        // SAFETY: the name is a valid, static, NUL-terminated UTF-16 string, no security
        // descriptor pointer is supplied, and ownership is unnecessary because object lifetime
        // (rather than mutex locking) implements the single-instance guard.
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\Raylander.Tray")) };
        // SAFETY: GetLastError has no preconditions and is read immediately after
        // CreateMutexW to distinguish a pre-existing named mutex.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let handle = handle.context("failed to create the Raylander single-instance mutex")?;
        if already_exists {
            // SAFETY: CreateMutexW returned this valid kernel handle.
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: this handle remains owned by the guard until drop.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct TrayIcon(NOTIFYICONDATAW);

impl TrayIcon {
    fn add(window: HWND) -> Result<Self> {
        let data = tray_icon_data(window)?;
        add_tray_icon_to_shell(&data)?;
        Ok(Self(data))
    }

    fn restore(window: HWND) -> Result<()> {
        add_tray_icon_to_shell(&tray_icon_data(window)?)
    }
}

fn tray_icon_data(window: HWND) -> Result<NOTIFYICONDATAW> {
    // SAFETY: IDI_APPLICATION is a stock resource and requires a null module handle.
    let icon =
        unsafe { LoadIconW(None, IDI_APPLICATION) }.context("failed to load the tray icon")?;
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK,
        hIcon: icon,
        ..Default::default()
    };
    copy_wide(&mut data.szTip, "Raylander");
    Ok(data)
}

fn add_tray_icon_to_shell(data: &NOTIFYICONDATAW) -> Result<()> {
    // SAFETY: data is fully initialized, has the documented size, and references a live window
    // owned by this process.
    if !unsafe { Shell_NotifyIconW(NIM_ADD, data) }.as_bool() {
        bail!("Windows rejected the Raylander notification-area icon");
    }
    Ok(())
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: this removes the icon identified by the same window and ID used for NIM_ADD.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.0) };
    }
}

struct MessageWindow(HWND);

impl MessageWindow {
    fn create() -> Result<Self> {
        // SAFETY: a null module name asks for the current executable module.
        let module =
            unsafe { GetModuleHandleW(None) }.context("failed to get the application module")?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("RaylanderTrayWindow"),
            ..Default::default()
        };
        // SAFETY: `class` points to initialized memory for the duration of the call and its string
        // fields have static lifetime.
        let atom = unsafe { RegisterClassW(&class) };
        if atom == 0 {
            // SAFETY: GetLastError has no preconditions and follows RegisterClassW immediately.
            let error = unsafe { GetLastError() };
            if error != ERROR_CLASS_ALREADY_EXISTS {
                bail!(
                    "failed to register the Raylander tray window class (Windows error {})",
                    error.0
                );
            }
        }

        // SAFETY: the registered class, static window title, instance, and optional arguments
        // satisfy CreateWindowExW. A zero-sized top-level window with no visible style is hidden.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("RaylanderTrayWindow"),
                w!("Raylander"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .context("failed to create the Raylander tray message window")?;
        Ok(Self(window))
    }

    fn handle(&self) -> HWND {
        self.0
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        // SAFETY: this guard owns the window created by CreateWindowExW. DestroyWindow safely
        // reports failure if WM_COMMAND already destroyed it before the message loop returned.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn run_message_loop() -> Result<()> {
    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is writable for the duration of the call; no window filter is used.
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
        match status.0 {
            -1 => {
                return Err(anyhow!(windows::core::Error::from_thread()))
                    .context("the Raylander tray message loop failed");
            }
            0 if message.wParam.0 == 0 => return Ok(()),
            0 => bail!("the Raylander tray window procedure stopped unexpectedly"),
            _ => {
                // SAFETY: GetMessageW populated a valid MSG.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Windows supplied the callback arguments and the inner procedure follows the
        // Win32 window-procedure contract.
        unsafe { window_proc_inner(window, message, wparam, lparam) }
    })) {
        Ok(result) => result,
        Err(_) => {
            // Never allow a Rust panic to unwind across the Win32 callback boundary.
            // SAFETY: posting WM_QUIT from the window's message thread is valid.
            unsafe { PostQuitMessage(1) };
            LRESULT(0)
        }
    }
}

unsafe fn window_proc_inner(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        TRAY_CALLBACK => match lparam.0 as u32 {
            WM_CONTEXTMENU | WM_RBUTTONUP => {
                if let Err(error) = show_context_menu(window) {
                    show_message("Raylander", &format!("{error:#}"), MB_ICONERROR);
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                report_action(open_dock());
                LRESULT(0)
            }
            _ => LRESULT(0),
        },
        OPEN_DOCK_MESSAGE => {
            report_action(open_dock());
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xffff) as u16 {
                MENU_OPEN_DOCK => report_action(open_dock()),
                MENU_OPEN_CONFIG => report_action(open_config_folder()),
                MENU_REINSTALL_HOOKS => match app::install_hooks() {
                    Ok(message) => show_message("Raylander", &message, MB_ICONINFORMATION),
                    Err(error) => {
                        report_action(Err(error.context("failed to reinstall Codex hooks")))
                    }
                },
                MENU_EXIT => {
                    // SAFETY: the handle is the live window receiving this message.
                    let _ = unsafe { DestroyWindow(window) };
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            // SAFETY: the handle is the live window receiving the session-ending notification.
            let _ = unsafe { DestroyWindow(window) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: posting WM_QUIT from the window's owning message thread is valid.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ if TASKBAR_CREATED_MESSAGE.get().copied() == Some(message) => {
            report_action(TrayIcon::restore(window));
            LRESULT(0)
        }
        _ => {
            // SAFETY: unhandled messages are delegated with their original parameters.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

fn show_context_menu(window: HWND) -> Result<()> {
    // SAFETY: CreatePopupMenu has no preconditions.
    let menu = unsafe { CreatePopupMenu() }.context("failed to create the tray menu")?;
    let result = (|| {
        // SAFETY: `menu` is live and all labels are static NUL-terminated strings.
        unsafe {
            AppendMenuW(menu, MF_STRING, MENU_OPEN_DOCK as usize, w!("Open Dock"))?;
            AppendMenuW(
                menu,
                MF_STRING,
                MENU_OPEN_CONFIG as usize,
                w!("Open Config Folder"),
            )?;
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
            AppendMenuW(
                menu,
                MF_STRING,
                MENU_REINSTALL_HOOKS as usize,
                w!("Reinstall Hooks"),
            )?;
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
            AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, w!("Exit"))?;
        }

        let mut point = POINT::default();
        // SAFETY: `point` is writable and `window` owns the menu interaction.
        unsafe {
            GetCursorPos(&mut point)?;
            let _ = SetForegroundWindow(window);
            let _ = TrackPopupMenu(
                menu,
                TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                window,
                None,
            );
            // Required by TrackPopupMenu so Windows reliably dismisses the menu.
            PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0))?;
        }
        Ok(())
    })();
    // SAFETY: this process owns the popup menu and TrackPopupMenu has returned.
    let _ = unsafe { DestroyMenu(menu) };
    result
}

fn register_taskbar_created_message() -> Result<()> {
    // SAFETY: the message name is a valid static, NUL-terminated UTF-16 string.
    let message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    anyhow::ensure!(
        message != 0,
        "failed to register the TaskbarCreated message"
    );
    let _ = TASKBAR_CREATED_MESSAGE.set(message);
    Ok(())
}

fn activate_existing_instance() -> Result<()> {
    for _ in 0..40 {
        // SAFETY: the class name is a static, NUL-terminated string and a null title matches any
        // window with that class.
        if let Ok(window) = unsafe { FindWindowW(w!("RaylanderTrayWindow"), PCWSTR::null()) } {
            // SAFETY: FindWindowW returned a live top-level window. PostMessageW copies the scalar
            // message parameters before returning.
            unsafe {
                PostMessageW(Some(window), OPEN_DOCK_MESSAGE, WPARAM(0), LPARAM(0))?;
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The first instance may still be starting or may have failed after acquiring the mutex.
    // Opening the configured URL is the least surprising fallback and does not start a daemon.
    open_dock()
}

fn open_dock() -> Result<()> {
    let url = match DOCK_URL.get() {
        Some(url) => url.clone(),
        None => app::configured_dock_url()?,
    };
    shell_open(&url)
}

fn open_config_folder() -> Result<()> {
    let path = config::config_path()?;
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(folder).with_context(|| format!("failed to create {}", folder.display()))?;
    shell_open(&folder.to_string_lossy())
}

fn shell_open(target: &str) -> Result<()> {
    let display_target = target.to_owned();
    let target = to_wide(target);
    // SAFETY: all pointers reference NUL-terminated strings that outlive the call. `open` asks
    // Windows Shell to use the registered handler for a URL or directory.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        bail!("Windows Shell could not open {display_target}");
    }
    Ok(())
}

fn report_action(result: Result<()>) {
    if let Err(error) = result {
        show_message("Raylander", &format!("{error:#}"), MB_ICONERROR);
    }
}

fn show_message(
    title: &str,
    message: &str,
    icon: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) {
    let title = to_wide(title);
    let message = to_wide(message);
    // SAFETY: both UTF-16 buffers are NUL terminated and remain live for the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | icon,
        );
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn copy_wide<const N: usize>(destination: &mut [u16; N], value: &str) {
    let encoded = value.encode_utf16();
    for (slot, code_unit) in destination
        .iter_mut()
        .zip(encoded)
        .take(N.saturating_sub(1))
    {
        *slot = code_unit;
    }
}

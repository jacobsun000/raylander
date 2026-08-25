#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    match raylander::run_tray() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            raylander::show_tray_startup_error(&format!("{error:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}

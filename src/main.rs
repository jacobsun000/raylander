fn main() -> std::process::ExitCode {
    match raylander::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("raylander: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

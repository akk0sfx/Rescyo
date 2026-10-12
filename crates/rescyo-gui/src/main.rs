//! `rescyo-gui`: the desktop application.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    let mut opts = rescyo_gui::Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lang" => opts.lang = args.next(),
            "--light" => opts.dark = Some(false),
            "--dark" => opts.dark = Some(true),
            "--version" | "-V" => {
                println!("rescyo-gui {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            other => opts.session = Some(std::path::PathBuf::from(other)),
        }
    }
    match rescyo_gui::run(opts) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rescyo-gui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

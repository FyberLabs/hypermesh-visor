//! Linux desktop companion. A bug on the desktop.
//! Sign-in stores a refresh token in the system keychain, not in the visor vault.

#[cfg(not(target_os = "linux"))]
compile_error!("the desktop companion is Linux only");

mod auth;
mod credentials;
mod draw;
mod pages;
mod settings;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => match window::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("hypermesh-companion: {err}");
                ExitCode::from(1)
            }
        },
        Some("--dump-frames") => {
            let Some(dir) = args.next() else {
                eprintln!("hypermesh-companion: --dump-frames needs a directory");
                return ExitCode::from(2);
            };
            if let Err(err) = draw::dump_frames(&PathBuf::from(dir)) {
                eprintln!("hypermesh-companion: {err}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            println!(
                "hypermesh-companion\n  --dump-frames DIR   write the idle and active bug frames"
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("hypermesh-companion: unknown argument {other}");
            ExitCode::from(2)
        }
    }
}

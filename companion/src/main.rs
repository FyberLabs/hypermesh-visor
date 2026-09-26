//! Linux desktop companion. A bug on the desktop.
//! Sign-in stores a refresh token in the system keychain, not in the visor vault.

#[cfg(not(target_os = "linux"))]
compile_error!("the desktop companion is Linux only");

mod auth;
mod credentials;
mod draw;
mod pages;
mod prompt;
mod settings;
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

fn visor_base() -> String {
    match std::env::var("HYPERMESH_VISOR_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => "http://127.0.0.1:9847".into(),
    }
}

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
        Some("--prompt") => {
            let text = match args.next() {
                Some(text) => text,
                None => {
                    eprintln!("hypermesh-companion: --prompt needs text");
                    return ExitCode::from(2);
                }
            };
            let mut session = None;
            let mut model = None;
            let mut visor = None;
            while let Some(flag) = args.next() {
                let value = match args.next() {
                    Some(value) => value,
                    None => {
                        eprintln!("hypermesh-companion: {flag} needs a value");
                        return ExitCode::from(2);
                    }
                };
                match flag.as_str() {
                    "--session" => session = Some(value),
                    "--model" => model = Some(value),
                    "--visor" => visor = Some(value),
                    other => {
                        eprintln!("hypermesh-companion: unknown argument {other}");
                        return ExitCode::from(2);
                    }
                }
            }
            let Some(session) = session else {
                eprintln!("hypermesh-companion: --session is required");
                return ExitCode::from(2);
            };
            let call = prompt::PromptCall {
                visor: visor.unwrap_or_else(visor_base),
                session,
                prompt: text,
                model,
            };
            let key = std::env::var("HYPERMESH_API_KEY").unwrap_or_default();
            match prompt::send_prompt(&call, &key) {
                Ok(body) => {
                    println!("{body}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("hypermesh-companion: {err}");
                    ExitCode::from(1)
                }
            }
        }
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
                "hypermesh-companion\n  --dump-frames DIR   write the idle and active bug frames\n  --prompt TEXT --session ID [--model NAME] [--visor URL]\n      send one prompt on the visor stream"
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("hypermesh-companion: unknown argument {other}");
            ExitCode::from(2)
        }
    }
}

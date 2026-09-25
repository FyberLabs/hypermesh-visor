use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hypermesh_visor::{current_euid, parse_listen, serve, AppState, LinuxDesktop};

fn main() -> ExitCode {
    let listen = match parse_args() {
        Ok(Some(listen)) => listen,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hypermesh-visor: {message}");
            return ExitCode::from(2);
        }
    };
    if current_euid() == 0 {
        eprintln!("hypermesh-visor: refusing to run as root");
        return ExitCode::from(1);
    }
    let addr = match parse_listen(&listen) {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("hypermesh-visor: {err}");
            return ExitCode::from(2);
        }
    };
    let state = Arc::new(AppState::new(
        Arc::new(LinuxDesktop::from_process()),
        Duration::from_millis(250),
    ));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("hypermesh-visor: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = runtime.block_on(serve(addr, state)) {
        eprintln!("hypermesh-visor: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn parse_args() -> Result<Option<String>, String> {
    let mut args = std::env::args().skip(1);
    let mut listen = "127.0.0.1:9847".to_string();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                listen = args
                    .next()
                    .ok_or_else(|| "--listen needs an address".to_string())?;
            }
            "-h" | "--help" => {
                println!("hypermesh-visor [--listen 127.0.0.1:9847]");
                let _ = io::stdout().flush();
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Some(listen))
}

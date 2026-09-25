use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hypermesh_visor::{
    current_euid, parse_listen, serve, AppState, Desktop, FixtureDesktop, LinuxDesktop,
};

struct Options {
    listen: String,
    fixture_log: Option<std::path::PathBuf>,
}

fn main() -> ExitCode {
    let options = match parse_args() {
        Ok(Some(options)) => options,
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
    let addr = match parse_listen(&options.listen) {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("hypermesh-visor: {err}");
            return ExitCode::from(2);
        }
    };
    let desktop: Arc<dyn Desktop> = if let Some(path) = &options.fixture_log {
        match FixtureDesktop::open(path) {
            Ok(desktop) => Arc::new(desktop),
            Err(err) => {
                eprintln!("hypermesh-visor: {err}");
                return ExitCode::from(1);
            }
        }
    } else {
        Arc::new(LinuxDesktop::from_process())
    };
    let state = Arc::new(AppState::new(desktop, Duration::from_millis(250)));
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

fn parse_args() -> Result<Option<Options>, String> {
    let mut args = std::env::args().skip(1);
    let mut listen = "127.0.0.1:9847".to_string();
    let mut fixture = false;
    let mut fixture_log = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                listen = args
                    .next()
                    .ok_or_else(|| "--listen needs an address".to_string())?;
            }
            "--fixture" => fixture = true,
            "--fixture-log" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--fixture-log needs a path".to_string())?;
                fixture_log = Some(std::path::PathBuf::from(path));
            }
            "-h" | "--help" => {
                println!(
                    "hypermesh-visor [--listen 127.0.0.1:9847] [--fixture --fixture-log PATH]"
                );
                let _ = io::stdout().flush();
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if fixture && fixture_log.is_none() {
        return Err("--fixture needs --fixture-log".into());
    }
    if fixture_log.is_some() && !fixture {
        return Err("--fixture-log needs --fixture".into());
    }
    Ok(Some(Options {
        listen,
        fixture_log,
    }))
}

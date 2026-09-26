use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hypermesh_visor::{
    current_euid, open_supervisor_door, parse_listen, serve, AppState, Desktop, FixtureDesktop,
    LinuxDesktop,
};

struct Options {
    listen: String,
    fixture_log: Option<std::path::PathBuf>,
    supervisor_url: Option<String>,
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
    let supervisor = resolve_supervisor_url(
        options.supervisor_url.as_deref(),
        std::env::var("HYPERMESH_SUPERVISOR_URL").ok().as_deref(),
    );
    let door = match open_supervisor_door(supervisor.as_deref()) {
        Ok(door) => door,
        Err(message) => {
            eprintln!("hypermesh-visor: {message}");
            return ExitCode::from(2);
        }
    };
    let state = Arc::new(AppState::with_prompt_door(
        desktop,
        Duration::from_millis(250),
        door,
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

fn parse_args() -> Result<Option<Options>, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> Result<Option<Options>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut listen = "127.0.0.1:9847".to_string();
    let mut fixture = false;
    let mut fixture_log = None;
    let mut supervisor_url = None;
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
            "--supervisor-url" => {
                let url = args
                    .next()
                    .ok_or_else(|| "--supervisor-url needs a URL".to_string())?;
                if url.trim().is_empty() {
                    return Err("--supervisor-url needs a URL".into());
                }
                supervisor_url = Some(url);
            }
            "-h" | "--help" => {
                println!(
                    "hypermesh-visor [--listen 127.0.0.1:9847] [--fixture --fixture-log PATH] [--supervisor-url URL]\n\
                     --supervisor-url or HYPERMESH_SUPERVISOR_URL points POST /prompt at the renter supervisor.\n\
                     Without that URL the daemon does not call a model. The org API key stays on each request."
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
        supervisor_url,
    }))
}

/// Flag wins. A blank value is not an opt-in, so the daemon stays unconfigured.
fn resolve_supervisor_url(flag: Option<&str>, env: Option<&str>) -> Option<String> {
    let chosen = flag.filter(|value| !value.trim().is_empty()).or(env)?;
    let trimmed = chosen.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_url_is_an_explicit_opt_in() {
        assert_eq!(resolve_supervisor_url(None, None), None);
        assert_eq!(resolve_supervisor_url(None, Some("  ")), None);
        assert_eq!(
            resolve_supervisor_url(None, Some(" http://127.0.0.1:9 ")).as_deref(),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            resolve_supervisor_url(Some("http://flag"), Some("http://env")).as_deref(),
            Some("http://flag")
        );
    }

    #[test]
    fn supervisor_url_flag_requires_a_url() {
        let err = parse_args_from(["--supervisor-url".to_string()]).unwrap_err();
        assert!(err.contains("--supervisor-url"));
        let options = parse_args_from([
            "--supervisor-url".to_string(),
            "http://127.0.0.1:9".to_string(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            options.supervisor_url.as_deref(),
            Some("http://127.0.0.1:9")
        );
    }
}

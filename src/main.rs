use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hypermesh_visor::{
    current_euid, parse_listen, serve, AppState, Desktop, DoorConfig, FixtureDesktop, LinuxDesktop,
    DEFAULT_CATALOG_ID, SHUTDOWN_DEADLINE,
};

#[derive(Debug)]
struct Options {
    tcp: bool,
    listen: String,
    fixture_log: Option<std::path::PathBuf>,
    supervisor_url: Option<String>,
    supervisor_url_2: Option<String>,
    default_model: Option<String>,
    routes: Vec<(String, String)>,
    experts: Vec<String>,
    answers: Vec<(String, String)>,
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
    let tcp = if tcp_enabled(options.tcp, std::env::var("HYPERMESH_TCP").ok().as_deref()) {
        match parse_listen(&options.listen) {
            Ok(addr) => Some(addr),
            Err(err) => {
                eprintln!("hypermesh-visor: {err}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
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
    let config = DoorConfig {
        default_model: resolve_default_model(
            options.default_model.as_deref(),
            std::env::var("HYPERMESH_DEFAULT_MODEL").ok().as_deref(),
        ),
        primary_url: resolve_supervisor_url(
            options.supervisor_url.as_deref(),
            std::env::var("HYPERMESH_SUPERVISOR_URL").ok().as_deref(),
        ),
        second_url: resolve_supervisor_url(
            options.supervisor_url_2.as_deref(),
            std::env::var("HYPERMESH_SUPERVISOR_URL_2").ok().as_deref(),
        ),
        routes: options.routes,
        experts: options.experts,
        answers: options.answers,
    };
    let state = match AppState::with_orchestrator(desktop, Duration::from_millis(250), config) {
        Ok(state) => Arc::new(state),
        Err(message) => {
            eprintln!("hypermesh-visor: {message}");
            return ExitCode::from(2);
        }
    };
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
    let result = runtime.block_on(serve(tcp, state));
    // Drop waits forever for a stuck spawn_blocking task. This bound is the
    // same deadline `serve` uses before it exits the process.
    runtime.shutdown_timeout(SHUTDOWN_DEADLINE);
    if let Err(err) = result {
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
    let mut tcp = false;
    let mut listen = "127.0.0.1:9847".to_string();
    let mut fixture = false;
    let mut fixture_log = None;
    let mut supervisor_url = None;
    let mut supervisor_url_2 = None;
    let mut default_model = None;
    let mut routes = Vec::new();
    let mut experts = Vec::new();
    let mut answers = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tcp" => tcp = true,
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
                supervisor_url = Some(required_value(&mut args, "--supervisor-url")?);
            }
            "--supervisor-url-2" => {
                supervisor_url_2 = Some(required_value(&mut args, "--supervisor-url-2")?);
            }
            "--default-model" => {
                default_model = Some(required_value(&mut args, "--default-model")?);
            }
            "--route" => {
                routes.push(split_pair(
                    &required_value(&mut args, "--route")?,
                    "--route",
                )?);
            }
            "--experts" => {
                experts.push(required_value(&mut args, "--experts")?);
            }
            "--expert-answer" => {
                answers.push(split_pair(
                    &required_value(&mut args, "--expert-answer")?,
                    "--expert-answer",
                )?);
            }
            "-h" | "--help" => {
                println!(
                    "hypermesh-visor [--tcp] [--listen 127.0.0.1:9847] [--fixture --fixture-log PATH]\n\
                     [--supervisor-url URL] [--supervisor-url-2 URL] [--default-model {DEFAULT_CATALOG_ID}]\n\
                     [--route CATALOG=URL] [--experts CATALOG] [--expert-answer CATALOG=URL]\n\
                     The control API listens on control.sock in the runtime directory.\n\
                     --tcp or HYPERMESH_TCP=1 also opens a loopback port. Every request on that\n\
                     port needs the bearer in api.token. --listen sets that port and must be loopback.\n\
                     --supervisor-url or HYPERMESH_SUPERVISOR_URL is the renter supervisor.\n\
                     --supervisor-url-2 or HYPERMESH_SUPERVISOR_URL_2 is a second host. A blank first URL stays unconfigured.\n\
                     --default-model or HYPERMESH_DEFAULT_MODEL is used when a prompt omits model.\n\
                     Two hosts need --route. --experts fans that catalog id across both hosts.\n\
                     The org API key stays on each request."
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
        tcp,
        listen,
        fixture_log,
        supervisor_url,
        supervisor_url_2,
        default_model,
        routes,
        experts,
        answers,
    }))
}

fn required_value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
    if value.trim().is_empty() {
        return Err(format!("{flag} needs a value"));
    }
    Ok(value)
}

fn split_pair(value: &str, flag: &str) -> Result<(String, String), String> {
    let Some((left, right)) = value.split_once('=') else {
        return Err(format!("{flag} needs CATALOG=URL"));
    };
    if left.trim().is_empty() || right.trim().is_empty() {
        return Err(format!("{flag} needs CATALOG=URL"));
    }
    Ok((left.trim().to_string(), right.trim().to_string()))
}

fn resolve_default_model(flag: Option<&str>, env: Option<&str>) -> String {
    let chosen = flag
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env.filter(|value| !value.trim().is_empty()));
    match chosen {
        Some(value) => value.trim().to_string(),
        None => DEFAULT_CATALOG_ID.to_string(),
    }
}

/// TCP is off unless `--tcp` is set or `HYPERMESH_TCP` is `1`, `true`, `yes`, or `on`.
fn tcp_enabled(flag: bool, env: Option<&str>) -> bool {
    if flag {
        return true;
    }
    matches!(
        env.map(str::trim)
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
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
    fn tcp_is_off_unless_the_flag_or_env_opts_in() {
        assert!(!tcp_enabled(false, None));
        assert!(!tcp_enabled(false, Some("")));
        assert!(!tcp_enabled(false, Some("0")));
        assert!(!tcp_enabled(false, Some("false")));
        assert!(!tcp_enabled(false, Some("no")));
        assert!(!tcp_enabled(false, Some("off")));
        assert!(tcp_enabled(true, None));
        assert!(tcp_enabled(false, Some("1")));
        assert!(tcp_enabled(false, Some(" true ")));
        assert!(tcp_enabled(false, Some("YES")));
        assert!(tcp_enabled(false, Some("on")));
        let options = parse_args_from([
            "--fixture".into(),
            "--fixture-log".into(),
            "events.jsonl".into(),
        ])
        .unwrap()
        .unwrap();
        assert!(!options.tcp);
        let options = parse_args_from(["--tcp".into()]).unwrap().unwrap();
        assert!(options.tcp);
        assert_eq!(options.listen, "127.0.0.1:9847");
    }

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

    #[test]
    fn default_model_flag_wins_and_a_blank_env_stays_the_catalog_id() {
        assert_eq!(resolve_default_model(None, None), DEFAULT_CATALOG_ID);
        assert_eq!(resolve_default_model(None, Some("  ")), DEFAULT_CATALOG_ID);
        assert_eq!(
            resolve_default_model(None, Some(" whisper-small ")),
            "whisper-small"
        );
        assert_eq!(
            resolve_default_model(Some("embed-minilm"), Some("whisper-small")),
            "embed-minilm"
        );
        let err = parse_args_from(["--default-model".to_string()]).unwrap_err();
        assert!(err.contains("--default-model"));
        let options = parse_args_from([
            "--default-model".to_string(),
            "whisper-small".to_string(),
            "--supervisor-url".to_string(),
            "http://127.0.0.1:9".to_string(),
            "--supervisor-url-2".to_string(),
            "http://127.0.0.1:8".to_string(),
            "--route".to_string(),
            "whisper-small=http://127.0.0.1:9".to_string(),
            "--experts".to_string(),
            "llama-3.1-8b-q4".to_string(),
            "--expert-answer".to_string(),
            "llama-3.1-8b-q4=http://127.0.0.1:8".to_string(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.default_model.as_deref(), Some("whisper-small"));
        assert_eq!(
            options.supervisor_url_2.as_deref(),
            Some("http://127.0.0.1:8")
        );
        assert_eq!(
            options.routes,
            vec![("whisper-small".into(), "http://127.0.0.1:9".into())]
        );
        assert_eq!(options.experts, vec!["llama-3.1-8b-q4".to_string()]);
        assert_eq!(
            options.answers,
            vec![("llama-3.1-8b-q4".into(), "http://127.0.0.1:8".into())]
        );
    }
}

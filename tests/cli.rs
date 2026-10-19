use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hypermesh-visor"))
}

#[test]
fn help_mentions_the_daemon() {
    let output = bin().arg("--help").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("hypermesh-visor"));
    assert!(text.contains("--listen"));
}

#[test]
fn fixture_needs_a_log_path() {
    let output = bin().arg("--fixture").output().unwrap();
    assert!(!output.status.success());
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("--fixture"));
}

#[test]
fn rejects_a_non_loopback_listen_address() {
    let output = bin()
        .args(["--tcp", "--listen", "0.0.0.0:9"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("loopback"));
}

#[test]
fn listens_on_the_control_socket_and_not_tcp() {
    let runtime = std::env::temp_dir().join(format!(
        "hypermesh-cli-listen-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&runtime);
    std::fs::create_dir_all(&runtime).unwrap();
    let mut child = bin()
        .env("HYPERMESH_RUNTIME_DIR", &runtime)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("control socket line");
    let listening = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "python3 -c 'import os,pathlib,sys\npid=int(sys.argv[1])\ninodes=set()\nfor entry in pathlib.Path(f\"/proc/{{pid}}/fd\").iterdir():\n    target=os.readlink(entry)\n    if target.startswith(\"socket:[\"): inodes.add(target[8:-1])\nfor name in (\"tcp\",\"tcp6\"):\n  path=pathlib.Path(\"/proc/net\")/name\n  if not path.exists(): continue\n  for line in path.read_text().splitlines()[1:]:\n    parts=line.split()\n    if len(parts)>9 and parts[3]==\"0A\" and parts[9] in inodes:\n      raise SystemExit(1)\n' {}",
            child.id()
        ))
        .status()
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&runtime);
    assert!(line.starts_with("control socket "), "stdout was {line:?}");
    assert!(listening.success(), "the daemon opened a TCP listener");
}

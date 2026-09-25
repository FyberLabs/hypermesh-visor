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
fn rejects_a_non_loopback_listen_address() {
    let output = bin().args(["--listen", "0.0.0.0:9"]).output().unwrap();
    assert!(!output.status.success());
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("loopback"));
}

#[test]
fn listens_on_loopback() {
    let mut child = bin()
        .args(["--listen", "127.0.0.1:0"])
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
        .expect("listen line");
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        line.starts_with("listening 127.0.0.1:"),
        "stdout was {line:?}"
    );
}

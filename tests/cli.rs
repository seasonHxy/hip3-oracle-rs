use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
const BIN: &str = env!("CARGO_BIN_EXE_hip3-oracle-rs");
fn fixture(dir: &Path, port: u16) -> std::path::PathBuf {
    let mut c: Value = serde_json::from_str(include_str!("../config/example.json")).unwrap();
    c["stateFile"] = json!(dir.join("oracle.json"));
    c["monitoring"]["port"] = json!(port);
    let path = dir.join("config.json");
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}
#[test]
fn cli_preflight_is_read_only_and_dry_run_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture(dir.path(), 0);
    let output = Command::new(BIN)
        .args(["--config", config.to_str().unwrap(), "preflight"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let output = Command::new(BIN)
        .args(["--config", config.to_str().unwrap(), "once"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(v["status"], "simulated");
    let output = Command::new(BIN)
        .args(["--config", config.to_str().unwrap(), "once", "--live"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
#[cfg(unix)]
#[tokio::test]
async fn monitoring_and_sigterm_finish_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let config = fixture(dir.path(), port);
    let mut child = Command::new(BIN)
        .args(["--config", config.to_str().unwrap(), "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    let mut ready = false;
    for _ in 0..50 {
        if let Ok(r) = client
            .get(format!("http://127.0.0.1:{port}/readyz"))
            .send()
            .await
            && r.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !ready {
        let _ = child.kill();
        panic!("readiness never became healthy");
    }
    let metrics = client
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("hip3_oracle_live_mode 0"));
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let mut exited = false;
    for _ in 0..50 {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !exited {
        let _ = child.kill();
        panic!("SIGTERM did not exit cleanly");
    }
}

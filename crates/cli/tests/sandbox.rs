//! End-to-end checks that the OS sandbox helper actually confines a command.
//! Linux-only (the helper applies Landlock + seccomp). Each test spawns the real
//! `opencli __sandbox …` helper as a child, so the restrictions apply to that
//! child and never to the test runner.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Output};

fn run_sandboxed(writable: &str, network: bool, command: &str) -> Output {
    let policy = format!(r#"{{"writable_roots":["{writable}"],"network":{network}}}"#);
    Command::new(env!("CARGO_BIN_EXE_opencli"))
        .args(["__sandbox", "--policy", &policy, "--", "sh", "-c", command])
        .output()
        .expect("spawn sandbox helper")
}

/// Landlock can be compiled in but disabled at boot (`lsm=` without it) or absent
/// on an old kernel. The filesystem-confinement assertions only hold when it is
/// actually active, so gate them on the kernel's reported LSM list.
fn landlock_active() -> bool {
    std::fs::read_to_string("/sys/kernel/security/lsm")
        .map(|s| s.split(',').any(|m| m.trim() == "landlock"))
        .unwrap_or(false)
}

fn tmp_workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opencli-sb-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn write_inside_workspace_succeeds() {
    let dir = tmp_workspace("in");
    let target = dir.join("ok.txt");
    let out = run_sandboxed(
        dir.to_str().unwrap(),
        false,
        &format!("echo hi > {}", target.display()),
    );
    assert!(
        out.status.success(),
        "write inside workspace should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(target.exists(), "file should exist inside the workspace");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn write_outside_workspace_is_denied() {
    if !landlock_active() {
        eprintln!("skipping: landlock not active on this kernel");
        return;
    }
    let dir = tmp_workspace("out");
    let probe = "/etc/opencli_sandbox_probe";
    let out = run_sandboxed(
        dir.to_str().unwrap(),
        false,
        &format!("echo x > {probe} 2>&1"),
    );
    assert!(
        !out.status.success(),
        "writing outside the workspace must fail under landlock"
    );
    assert!(
        !Path::new(probe).exists(),
        "the outside file must not exist"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dev_null_redirect_works() {
    let dir = tmp_workspace("devnull");
    let out = run_sandboxed(dir.to_str().unwrap(), false, "echo hi > /dev/null");
    assert!(
        out.status.success(),
        "redirect to /dev/null must work; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn inet_socket_blocked_unix_allowed_when_network_off() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let dir = tmp_workspace("net");
    let inet = run_sandboxed(
        dir.to_str().unwrap(),
        false,
        "python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)'",
    );
    assert!(
        !inet.status.success(),
        "AF_INET socket must be blocked when network is off"
    );
    let unix = run_sandboxed(
        dir.to_str().unwrap(),
        false,
        "python3 -c 'import socket; socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)'",
    );
    assert!(
        unix.status.success(),
        "AF_UNIX socket must still be allowed; stderr: {}",
        String::from_utf8_lossy(&unix.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn network_allowed_lets_inet_socket_open() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let dir = tmp_workspace("neton");
    // Creating the socket (not connecting) must succeed when network is allowed.
    let out = run_sandboxed(
        dir.to_str().unwrap(),
        true,
        "python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)'",
    );
    assert!(
        out.status.success(),
        "AF_INET socket must be allowed when network=true; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

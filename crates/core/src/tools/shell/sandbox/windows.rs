//! Windows best-effort sandbox: run the command inside a Job Object so the whole
//! process tree is cleaned up reliably. Unlike Landlock (Linux) / `sandbox-exec`
//! (macOS) this does NOT confine the filesystem or network — Windows has no
//! equally cheap mechanism — so `doctor` keeps reporting the platform as
//! unsandboxed. Its purpose is teardown: `run_shell`'s timeout and
//! `kill_on_drop` only kill the immediate child on Windows (there are no Unix
//! process groups), so a backgrounded grandchild could leak. A Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates every process in the job when
//! the handle closes — i.e. when the helper finishes or is itself killed.
//!
//! Symmetric with the Linux helper: [`wrap`] (parent side) re-execs us as
//! `<exe> __sandbox --policy <json> -- cmd /C <command>`; [`run_helper`] (child
//! side) creates the job, spawns the command, assigns the child to the job, then
//! waits and propagates the exit code. The policy's `writable_roots`/`network`
//! fields are accepted for protocol symmetry but not enforced here. Any Job API
//! failure falls back to running the command unsandboxed rather than failing it.

use std::ffi::OsString;
use std::os::windows::io::AsRawHandle;

use tokio::process::Command;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use super::SandboxPolicy;

/// Parent side: build a command that re-execs us as the sandbox helper, which
/// runs `cmd /C <command>` inside a Job Object. `None` if we can't find our own
/// executable (the caller then runs the command unsandboxed).
pub(super) fn wrap(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    let exe = std::env::current_exe().ok()?;
    let policy_json = serde_json::to_string(policy).ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__sandbox")
        .arg("--policy")
        .arg(policy_json)
        .arg("--")
        .arg("cmd")
        .arg("/C")
        .arg(command);
    Some(cmd)
}

/// Child side: parse the helper argv (positioned AFTER `__sandbox`), run the
/// target inside a Job Object, and return its exit code. Only returns once the
/// target has finished.
pub(super) fn run_helper(mut args: std::env::ArgsOs) -> i32 {
    // The `--policy` value is accepted for protocol symmetry with Linux but not
    // enforced on Windows; we only need the target after `--`.
    let mut target: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        if a == "--" {
            target.extend(args.by_ref());
            break;
        }
    }
    if target.is_empty() {
        eprintln!("tomte sandbox: no target command after `--`");
        return 126;
    }
    run_in_job(&target)
}

/// Spawn the target, assign it to a `KILL_ON_JOB_CLOSE` job, wait, and return the
/// exit code. Best-effort: if the job can't be created or assigned, the command
/// still runs (just without guaranteed tree cleanup).
fn run_in_job(target: &[OsString]) -> i32 {
    let job = create_kill_on_close_job();

    let mut child = match std::process::Command::new(&target[0])
        .args(&target[1..])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tomte sandbox: failed to run command: {e}");
            return 127;
        }
    };

    let Some(job) = job else {
        return wait_code(&mut child);
    };

    // SAFETY: `child` is alive (not yet waited), so its process handle is valid.
    // Assigning after spawn leaves a tiny window before the child could spawn its
    // own children; acceptable for best-effort cleanup. Ignore failure — the
    // command still runs, just without guaranteed teardown.
    let _ = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
    let code = wait_code(&mut child);
    // Closing the last job handle triggers KILL_ON_JOB_CLOSE, terminating any
    // grandchildren that outlived the child. The helper is NOT a member of the
    // job, so its own exit code is unaffected.
    // SAFETY: a handle we created and have not yet closed.
    unsafe { CloseHandle(job) };
    code
}

fn wait_code(child: &mut std::process::Child) -> i32 {
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("tomte sandbox: failed to wait for command: {e}");
            1
        }
    }
}

/// Create an unnamed Job Object whose only limit is `KILL_ON_JOB_CLOSE`. We add
/// no resource caps (CPU/memory/process count): mirroring the conservative Linux
/// posture, those routinely break legitimate builds. `None` on any failure.
fn create_kill_on_close_job() -> Option<HANDLE> {
    // SAFETY: a null name and null attributes are valid; returns a new handle or
    // null on failure.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return None;
    }
    // SAFETY: this limit struct is plain-old-data, so an all-zero value is valid.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` matches the JobObjectExtendedLimitInformation class and the
    // byte length we pass; the pointer is read-only and not retained past the call.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(info) as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        // SAFETY: closing the handle we just created.
        unsafe { CloseHandle(job) };
        return None;
    }
    Some(job)
}

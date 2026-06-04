//! Linux sandbox enforcement.
//!
//! Two roles, both compiled into the single binary:
//!   - [`wrap`] (parent side): rebuilds the shell command to re-exec this binary
//!     as the sandbox helper.
//!   - [`run_helper`] (child side): inside the freshly-launched, single-threaded
//!     helper process, applies Landlock (filesystem + TCP) and seccomp (block
//!     `AF_INET`/`AF_INET6` sockets) to itself, then `execvp`s the real shell.
//!     Landlock domains and seccomp filters are inherited across `execve`, so
//!     the shell and every descendant stay confined.

use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use super::SandboxPolicy;

/// Parent side: build a command that re-execs us as the sandbox helper, which
/// enforces `policy` then runs `sh -c <command>`. `None` if we can't find our
/// own executable (the caller falls back to running unsandboxed).
pub(super) fn wrap(command: &str, policy: &SandboxPolicy) -> Option<Command> {
    let exe = std::env::current_exe().ok()?;
    let policy_json = serde_json::to_string(policy).ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__sandbox")
        .arg("--policy")
        .arg(policy_json)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(command);
    Some(cmd)
}

/// Child side: parse the helper argv (positioned AFTER `__sandbox`), enforce the
/// policy, and exec the target. Returns a process exit code; only returns on
/// failure (success replaces the image via `execvp`).
pub(super) fn run_helper(mut args: std::env::ArgsOs) -> i32 {
    let mut policy_json: Option<OsString> = None;
    let mut target: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        if a == "--policy" {
            policy_json = args.next();
        } else if a == "--" {
            target.extend(args.by_ref());
            break;
        }
    }
    match enforce_and_exec(policy_json, target) {
        Ok(()) => 0, // unreachable: a successful exec never returns
        Err(e) => {
            eprintln!("tomte sandbox: {e:#}");
            126
        }
    }
}

fn enforce_and_exec(policy_json: Option<OsString>, target: Vec<OsString>) -> Result<()> {
    let policy_json = policy_json.ok_or_else(|| anyhow!("missing --policy"))?;
    let policy: SandboxPolicy = serde_json::from_slice(policy_json.as_os_str().as_bytes())
        .context("invalid sandbox policy")?;
    if target.is_empty() {
        return Err(anyhow!("no target command after `--`"));
    }
    // 1) NO_NEW_PRIVS — required before seccomp; harmless for Landlock.
    set_no_new_privs()?;
    // 2) Landlock (filesystem, plus TCP when network is denied).
    apply_landlock(&policy)?;
    // 3) seccomp: block AF_INET/AF_INET6 sockets when network is denied.
    if !policy.network {
        block_inet_sockets()?;
    }
    // 4) exec the target — replaces the image; restrictions persist.
    exec(&target)
}

fn set_no_new_privs() -> Result<()> {
    // SAFETY: prctl with scalar arguments and no pointer/memory effects.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(anyhow::Error::from(std::io::Error::last_os_error()))
            .context("prctl(PR_SET_NO_NEW_PRIVS)");
    }
    Ok(())
}

/// Device nodes that stay writable in every mode — ordinary commands rely on
/// `> /dev/null`, `/dev/tty`, `/dev/urandom`, etc., and writing to them does not
/// mutate persistent state. Real block devices (e.g. `/dev/sda`) are NOT listed,
/// so the sandbox still prevents disk clobbering. Missing nodes are skipped.
const WRITABLE_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
    "/dev/shm",
];

fn apply_landlock(policy: &SandboxPolicy) -> Result<()> {
    use landlock::{
        Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset, RulesetAttr,
        RulesetCreatedAttr, ABI,
    };

    // Highest ABI we target; best-effort silently degrades on older kernels.
    let abi = ABI::V5;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| anyhow!("landlock handle fs access: {e}"))?;
    // Govern TCP only when we intend to deny it. With network allowed we don't
    // handle AccessNet at all, leaving sockets unaffected by Landlock.
    if !policy.network {
        ruleset = ruleset
            .handle_access(AccessNet::from_all(abi))
            .map_err(|e| anyhow!("landlock handle net access: {e}"))?;
    }
    let mut created = ruleset
        .create()
        .map_err(|e| anyhow!("landlock create ruleset: {e}"))?;
    // Read (and execute) everywhere.
    created = created
        .add_rules(landlock::path_beneath_rules(
            ["/"],
            AccessFs::from_read(abi),
        ))
        .map_err(|e| anyhow!("landlock read rule: {e}"))?;
    // Write only beneath the policy's writable roots (none ⇒ effectively read-only).
    if !policy.writable_roots.is_empty() {
        created = created
            .add_rules(landlock::path_beneath_rules(
                policy.writable_roots.iter(),
                AccessFs::from_all(abi),
            ))
            .map_err(|e| anyhow!("landlock write rules: {e}"))?;
    }
    // Standard device nodes stay writable in every mode (e.g. `> /dev/null`).
    created = created
        .add_rules(landlock::path_beneath_rules(
            WRITABLE_DEVICES,
            AccessFs::from_write(abi),
        ))
        .map_err(|e| anyhow!("landlock device rules: {e}"))?;
    let status = created
        .restrict_self()
        .map_err(|e| anyhow!("landlock restrict_self: {e}"))?;
    tracing::debug!(ruleset = ?status.ruleset, "landlock applied");
    Ok(())
}

fn block_inet_sockets() -> Result<()> {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule,
    };

    // socket(domain, type, protocol): `domain` is arg0, a scalar, so seccomp can
    // compare it. Deny AF_INET/AF_INET6; AF_UNIX and all other syscalls pass.
    let socket_rule = |family: i64| -> Result<SeccompRule> {
        let cond =
            SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, family as u64)
                .map_err(|e| anyhow!("seccomp condition: {e}"))?;
        SeccompRule::new(vec![cond]).map_err(|e| anyhow!("seccomp rule: {e}"))
    };

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    rules.insert(
        libc::SYS_socket,
        vec![
            socket_rule(libc::AF_INET as i64)?,
            socket_rule(libc::AF_INET6 as i64)?,
        ],
    );

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow, // mismatch_action: allow all other (non-socket) syscalls
        SeccompAction::Errno(libc::EACCES as u32), // match_action: deny AF_INET/AF_INET6 socket()
        std::env::consts::ARCH
            .try_into()
            .map_err(|e| anyhow!("seccomp target arch: {e}"))?,
    )
    .map_err(|e| anyhow!("seccomp filter: {e}"))?;

    let program: BpfProgram = filter
        .try_into()
        .map_err(|e| anyhow!("seccomp compile: {e}"))?;
    seccompiler::apply_filter(&program).map_err(|e| anyhow!("seccomp apply: {e}"))?;
    Ok(())
}

fn exec(argv: &[OsString]) -> Result<()> {
    let cstrs: Vec<CString> = argv
        .iter()
        .map(|s| CString::new(s.as_bytes()))
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| anyhow!("command argument contains an interior NUL byte"))?;
    let mut ptrs: Vec<*const libc::c_char> = cstrs.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    // SAFETY: `ptrs` is NUL-terminated and every pointer borrows a `CString` in
    // `cstrs`, which outlives this call. `execvp` only returns on error.
    unsafe {
        libc::execvp(ptrs[0], ptrs.as_ptr());
    }
    Err(anyhow::Error::from(std::io::Error::last_os_error())).context("execvp")
}

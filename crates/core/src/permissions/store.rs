//! On-disk permission storage: reading/merging the in-repo and user-level
//! files and persisting allow grants. Split out of `permissions`; logic unchanged.

use std::{
    io,
    path::{Path, PathBuf},
};

use super::{permissions_path, ProjectPermissions};

fn invalid_project_permissions_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn validate_existing_permissions_path(cwd: &Path) -> io::Result<()> {
    let dir = cwd.join(".tomte");
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(invalid_project_permissions_path(
                    "project permissions directory must not be a symlink",
                ));
            }
            if !meta.is_dir() {
                return Err(invalid_project_permissions_path(
                    "project permissions path must be a directory",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }

    let path = permissions_path(cwd);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(invalid_project_permissions_path(
                    "project permissions file must not be a symlink",
                ));
            }
            if !meta.is_file() {
                return Err(invalid_project_permissions_path(
                    "project permissions path must be a file",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

fn write_permissions_file(path: &Path, text: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}

/// Stable per-project filename for the user-level allow store. Canonicalizes so
/// different spellings of the same directory map to one file; falls back to the
/// path as given when it doesn't exist yet. FNV-1a keeps the mapping stable
/// across runs without pulling in a hashing dependency.
fn project_key(cwd: &Path) -> String {
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canon.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Directory holding the user-level (out-of-repo) allow grants, one file per
/// project, under the owner-only config dir so a cloned repo can't seed it.
fn user_permissions_dir() -> PathBuf {
    crate::config::config_dir().join("project-permissions")
}

pub(super) fn user_permissions_path(cwd: &Path) -> PathBuf {
    user_permissions_dir().join(format!("{}.json", project_key(cwd)))
}

/// Read and parse one permissions file; a missing, oversized, or malformed file
/// is empty. The read goes through the shared size cap (like every other
/// untrusted in-repo file: sessions, skills, subagents, project config) so a
/// hostile `.tomte/permissions.json` can't force a huge read/parse on every
/// tool call (`load` runs per call); a real permissions file is a few rules.
pub(super) fn read_permissions_at(path: &Path) -> ProjectPermissions {
    const MAX_PERMISSIONS_BYTES: u64 = 64 * 1024;
    match crate::config::read_text_file_capped(path, MAX_PERMISSIONS_BYTES) {
        Ok(text) => serde_json::from_str(crate::config::strip_bom(&text)).unwrap_or_default(),
        Err(_) => ProjectPermissions::default(),
    }
}

/// The in-repo `<cwd>/.tomte/permissions.json`. Symlinked dir/file is treated
/// as empty so a project link can't redirect the read.
pub(super) fn load_project_file(cwd: &Path) -> ProjectPermissions {
    if validate_existing_permissions_path(cwd).is_err() {
        return ProjectPermissions::default();
    }
    read_permissions_at(&permissions_path(cwd))
}

/// Merge in-repo project rules with the user's own grants. The repo file is
/// honored for `deny` ONLY (an untrusted clone may tighten, never grant); the
/// user-level store is the sole source of `allow`. Deny rules are unioned.
pub(super) fn merge_permissions(
    project: ProjectPermissions,
    user: ProjectPermissions,
) -> ProjectPermissions {
    let mut deny = project.deny;
    for d in user.deny {
        if !deny.contains(&d) {
            deny.push(d);
        }
    }
    ProjectPermissions {
        allow: user.allow,
        deny,
    }
}

/// Append `rule` to the allow-list file at `path` (idempotent), creating its
/// parent directory owner-only and refusing a symlinked directory or file (the
/// `O_NOFOLLOW` open in [`write_permissions_file`] rejects a symlinked file).
pub(super) fn add_allow_rule_at(path: &Path, rule: String) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid_project_permissions_path(
                    "user permissions directory must not be a symlink",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                crate::config::create_dir_secure(dir)?;
            }
            Err(e) => return Err(e),
        }
    }
    let mut perms = read_permissions_at(path);
    if !perms.allow.iter().any(|r| r == &rule) {
        perms.allow.push(rule);
    }
    let text = serde_json::to_string_pretty(&perms).unwrap_or_default();
    write_permissions_file(path, &text)
}

#[cfg(test)]
mod tests {
    use super::super::{decide, is_allowed, load, permissions_path, Decision, ProjectPermissions};
    use super::*;
    use serde_json::json;

    #[test]
    fn user_allow_store_persists_and_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!(
            "tomte-perm-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = tmp.join("store").join("proj.json");
        add_allow_rule_at(&path, "run_shell(cargo:*)".to_string()).unwrap();
        // Re-adding the same rule does not duplicate it.
        add_allow_rule_at(&path, "run_shell(cargo:*)".to_string()).unwrap();
        let perms = read_permissions_at(&path);
        assert_eq!(perms.allow, vec!["run_shell(cargo:*)".to_string()]);
        assert!(is_allowed(
            &perms,
            "run_shell",
            &json!({"command": "cargo run"})
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn project_file_allow_is_ignored_but_deny_is_honored() {
        // A cloned repo's `.tomte/permissions.json` may tighten (deny) but must
        // not silently grant (allow) — that is the whole point of the user store.
        let tmp = std::env::temp_dir().join(format!(
            "tomte-perm-trust-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".tomte")).unwrap();
        std::fs::write(
            permissions_path(&tmp),
            r#"{"allow":["write_file","run_shell(curl:*)"],"deny":["run_shell(rm:*)"]}"#,
        )
        .unwrap();
        let perms = load(&tmp);
        // allow from the repo file is dropped...
        assert!(perms.allow.is_empty(), "repo allow must be ignored");
        assert_eq!(
            decide(&perms, "write_file", &json!({"path": "x"})),
            Decision::Ask
        );
        // ...but deny is still honored.
        assert_eq!(
            decide(&perms, "run_shell", &json!({"command": "rm -rf /"})),
            Decision::Deny
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bom_prefixed_permissions_file_still_parses() {
        // A Windows editor adding a UTF-8 BOM must not silently turn the file
        // into "empty" — that would drop a repo's deny rules.
        let tmp = std::env::temp_dir().join(format!(
            "tomte-perm-bom-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("permissions.json");
        std::fs::write(&path, "\u{feff}{\"deny\":[\"run_shell(rm:*)\"]}").unwrap();
        let perms = read_permissions_at(&path);
        assert_eq!(perms.deny, vec!["run_shell(rm:*)".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn oversized_project_permissions_file_is_ignored() {
        // A hostile in-repo permissions.json that is pathologically large must be
        // ignored (read through the shared size cap), not read and parsed in full
        // on every tool call. Otherwise its rules would still load and feed the
        // O(pattern·text) glob matcher.
        let tmp = std::env::temp_dir().join(format!(
            "tomte-perm-huge-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".tomte")).unwrap();
        let giant = "a".repeat(200 * 1024);
        let json = format!(r#"{{"deny":["write_file({giant})"]}}"#);
        std::fs::write(permissions_path(&tmp), json).unwrap();
        let perms = load(&tmp);
        assert!(
            perms.deny.is_empty(),
            "oversized permissions file must be ignored"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn merge_takes_user_allow_and_unions_deny() {
        let project = ProjectPermissions {
            allow: vec!["write_file".into()],
            deny: vec!["run_shell(rm:*)".into()],
        };
        let user = ProjectPermissions {
            allow: vec!["run_shell(cargo:*)".into()],
            deny: vec!["run_shell(rm:*)".into(), "edit_file".into()],
        };
        let merged = merge_permissions(project, user);
        assert_eq!(merged.allow, vec!["run_shell(cargo:*)".to_string()]);
        assert_eq!(
            merged.deny,
            vec!["run_shell(rm:*)".to_string(), "edit_file".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_allow_rule_rejects_symlinked_store_dir() {
        use std::os::unix::fs::symlink;

        let base =
            std::env::temp_dir().join(format!("tomte-perm-dir-link-{}", rand::random::<u64>()));
        let outside =
            std::env::temp_dir().join(format!("tomte-perm-dir-target-{}", rand::random::<u64>()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let linked = base.join("store");
        symlink(&outside, &linked).unwrap();

        let err = add_allow_rule_at(&linked.join("proj.json"), "write_file".to_string())
            .expect_err("symlinked store directory must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !outside.join("proj.json").exists(),
            "must not write through a symlinked store directory"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn add_allow_rule_rejects_symlinked_target_file() {
        use std::os::unix::fs::symlink;

        let base =
            std::env::temp_dir().join(format!("tomte-perm-file-link-{}", rand::random::<u64>()));
        let outside =
            std::env::temp_dir().join(format!("tomte-perm-file-target-{}", rand::random::<u64>()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&outside);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&outside, "sentinel").unwrap();
        let target = base.join("proj.json");
        symlink(&outside, &target).unwrap();

        // The parent dir is real, so the dir check passes; the O_NOFOLLOW open of
        // the symlinked file then fails (ELOOP) without overwriting the target.
        let err = add_allow_rule_at(&target, "write_file".to_string())
            .expect_err("symlinked target file must be rejected");
        let _ = err;
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "sentinel",
            "must not overwrite the symlink target"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&outside);
    }
}

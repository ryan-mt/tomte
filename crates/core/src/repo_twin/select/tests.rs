
use super::*;
use crate::repo_twin::build;

/// A small TS project mirroring the pitch: a session importing a User type,
/// a test covering the session, and a legacy file that nothing reaches.
fn fixture() -> (tempfile::TempDir, RepoTwin) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src/auth")).unwrap();
    std::fs::create_dir_all(root.join("src/db")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
            root.join("src/auth/session.ts"),
            "import { User } from '../db/user';\nexport function createSession(u: User) { return u; }\n",
        )
        .unwrap();
    std::fs::write(
        root.join("src/db/user.ts"),
        "export interface User { id: string }\n",
    )
    .unwrap();
    std::fs::write(
            root.join("tests/auth.test.ts"),
            "import { createSession } from '../src/auth/session';\ntest('x', () => createSession({id:'1'}));\n",
        )
        .unwrap();
    std::fs::write(
        root.join("src/auth/auth-old.ts"),
        "export function legacyLogin() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("AGENTS.md"), "- session tokens expire in 1h\n").unwrap();
    let twin = build(root).unwrap();
    (tmp, twin)
}

#[test]
fn file_seed_pulls_deps_dependents_and_tests() {
    let (tmp, twin) = fixture();
    let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
    assert_eq!(sel.seed_kind, "file");
    assert_eq!(sel.resolved_seeds[0].path, "src/auth/session.ts");

    let paths: Vec<&str> = sel.selected.iter().map(|s| s.path.as_str()).collect();
    // Dependency: session imports the User type's file.
    assert!(paths.contains(&"src/db/user.ts"), "deps: {paths:?}");
    // Dependent + test: the test imports the session.
    assert!(paths.contains(&"tests/auth.test.ts"), "tests: {paths:?}");

    // The User-type file's reason names the import edge.
    let user = sel
        .selected
        .iter()
        .find(|s| s.path == "src/db/user.ts")
        .unwrap();
    assert!(user.reasons.iter().any(|r| r.source == "import"));
}

#[test]
fn legacy_neighbor_is_listed_as_ignored() {
    let (tmp, twin) = fixture();
    let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
    let ignored: Vec<&str> = sel.ignored.iter().map(|f| f.path.as_str()).collect();
    assert!(
        ignored.contains(&"src/auth/auth-old.ts"),
        "ignored: {ignored:?}"
    );
    let legacy = sel
        .ignored
        .iter()
        .find(|f| f.path == "src/auth/auth-old.ts")
        .unwrap();
    assert!(legacy.reason.contains("superseded"));
}

#[test]
fn symbol_seed_resolves_to_its_definition_file() {
    let (tmp, twin) = fixture();
    let sel = why_context(&twin, tmp.path(), "User");
    assert_eq!(sel.seed_kind, "symbol");
    assert_eq!(sel.resolved_seeds[0].path, "src/db/user.ts");
    // The session references `User`, so it's selected via the symbol graph.
    let session = sel
        .selected
        .iter()
        .find(|s| s.path == "src/auth/session.ts");
    assert!(session.is_some(), "session should reference User");
    assert!(session
        .unwrap()
        .reasons
        .iter()
        .any(|r| r.source == "symbol" || r.source == "import"));
}

#[test]
fn conventions_surface_for_the_seed() {
    let (tmp, twin) = fixture();
    let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
    // The AGENTS.md rule mentioning "session" is surfaced.
    assert!(sel.rules.iter().any(|r| r.text.contains("session tokens")));
}

#[test]
fn absolute_stack_trace_seed_resolves_via_root_strip() {
    let (tmp, twin) = fixture();
    // A pasted stack-trace location is absolute; the engine must map it back
    // onto the twin's root-relative paths instead of reporting "missing".
    let abs = format!("{}/src/auth/session.ts:2", twin.root);
    let sel = why_context(&twin, tmp.path(), &abs);
    assert_eq!(sel.seed_kind, "file", "seed: {abs}");
    assert_eq!(sel.resolved_seeds[0].path, "src/auth/session.ts");
}

#[test]
fn rule_token_attribution_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src/auth")).unwrap();
    std::fs::write(
        root.join("src/auth/session.ts"),
        "export function makeSession() {}\n",
    )
    .unwrap();
    // The rule mentions BOTH the dir token (`auth`) and the stem
    // (`session`); the card must always cite the same one (the min).
    std::fs::write(root.join("AGENTS.md"), "- auth session rules apply\n").unwrap();
    let twin = build(root).unwrap();
    for _ in 0..12 {
        let sel = why_context(&twin, root, "src/auth/session.ts");
        let rule = sel
            .rules
            .iter()
            .find(|r| r.text.contains("auth session"))
            .expect("rule surfaced");
        assert_eq!(rule.why, "mentions `auth`");
    }
}

#[test]
fn missing_seed_is_reported_not_panicked() {
    let (tmp, twin) = fixture();
    let sel = why_context(&twin, tmp.path(), "doesNotExistAnywhere");
    assert_eq!(sel.seed_kind, "missing");
    assert!(sel.notes.iter().any(|n| n.contains("could not resolve")));
    // Rendering a missing selection is still valid text.
    assert!(render(&sel).contains("Could not resolve"));
}

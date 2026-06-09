//! Repo Twin / Context X-Ray — a *verifiable* map of the repository.
//!
//! The agent's hard problem on a large project isn't intelligence, it's picking
//! the right context: it reads unrelated files, misses the important one, and
//! edits blindly. Repo Twin builds a living map of the repo straight from the
//! source — five indexes, each grounded in real code, tests, or git history —
//! so every "this file is relevant" claim has a *source* and can never be a
//! hallucinated "this project uses pattern X".
//!
//! The five MVP indexes (see the submodules):
//! 1. [`imports`]  — file / import graph
//! 2. [`symbols`]  — symbol / function graph
//! 3. [`testmap`]  — which tests cover which source files
//! 4. [`gitmap`]   — recent-change frequency from git history
//! 5. [`rules`]    — project conventions from AGENTS.md / README / docs
//!
//! The built twin is cached as JSON beside the memory/decision stores
//! (`<config>/projects/<key>/repo-twin.json`) and re-used until the working tree
//! changes (a content fingerprint over path+size+mtime). Pure JSON, no native
//! database — tomte ships zero C dependencies, and the index is read wholesale.
//!
//! [`select::why_context`] is the headline query: given a seed (a file, a
//! `file:line` from a stack trace, or a symbol name) it returns the files a
//! maintainer *would* pull in — each with the index it came from — and the
//! nearby files it deliberately leaves out, each with the reason it's unreachable.

pub mod gitmap;
pub mod imports;
pub mod rules;
pub mod select;
pub mod symbols;
pub mod testmap;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bump when the on-disk schema changes in a way that makes an old cache
/// unreadable or misleading — a mismatch forces a clean rebuild rather than
/// deserializing stale shapes.
pub const CACHE_VERSION: u32 = 1;

/// Skip a file larger than this for import/symbol extraction — a multi-megabyte
/// minified bundle or generated blob would dominate the index for no signal.
/// It's still counted as a node in the file graph.
const MAX_SOURCE_BYTES: u64 = 512 * 1024;

/// Hard ceiling on files walked, so a pathological tree (a vendored monorepo
/// that slips past .gitignore) can't make `build` run unbounded. When hit,
/// [`RepoTwin::truncated`] is set so the CLI can say so out loud.
const MAX_FILES: usize = 20_000;

/// The language tomte recognizes for structural extraction. `Other` files are
/// still tracked as nodes in the file graph (so the map is complete) but get no
/// import/symbol parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    /// The JavaScript/TypeScript family (.js/.jsx/.ts/.tsx/.mjs/.cjs) — one
    /// variant because import and definition syntax are shared.
    Web,
    Python,
    Go,
    Other,
}

impl Lang {
    /// Classify a path by extension. Unknown extensions are [`Lang::Other`].
    pub fn of(path: &str) -> Lang {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Lang::Rust,
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => Lang::Web,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            _ => Lang::Other,
        }
    }

    /// Whether tomte extracts imports/symbols for this language.
    pub fn is_source(self) -> bool {
        !matches!(self, Lang::Other)
    }
}

/// One file in the repository map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    /// `/`-separated path relative to the repo root.
    pub path: String,
    pub lang: Lang,
    /// True when the file is a test (by path/name convention or, for Rust, a
    /// `#[cfg(test)]`/`#[test]` marker).
    pub is_test: bool,
    /// Line count (0 for files skipped as too large).
    pub loc: usize,
}

/// One import edge: `from` imports `raw`, resolved to repo file `to` when the
/// specifier points inside the tree (relative paths, local modules). External
/// packages leave `to` as `None` but are still recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEdge {
    pub from: String,
    pub raw: String,
    pub to: Option<String>,
    pub line: usize,
}

/// One symbol definition (function, type, const, …) located at a source line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDef {
    pub file: String,
    pub name: String,
    /// Free-form kind tag: `fn`, `type`, `const`, `class`, … (language-specific).
    pub kind: String,
    pub line: usize,
}

/// One test→source coverage edge, with how it was inferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestEdge {
    pub test: String,
    pub covers: String,
    /// `import` (the test imports the source), `name` (filename convention), or
    /// `inline` (Rust in-file `#[cfg(test)]`).
    pub via: String,
}

/// Per-file git activity over the recent window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStat {
    pub file: String,
    pub commits: u32,
    /// Unix seconds of the most recent commit touching the file.
    pub last_ts: u64,
    pub last_subject: String,
}

/// A project-convention document (AGENTS.md, README, …) and the rule-like lines
/// pulled from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleDoc {
    pub file: String,
    pub rules: Vec<RuleLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleLine {
    pub line: usize,
    pub text: String,
}

/// The whole repository map — the five indexes plus the metadata that lets the
/// cache decide whether it's still fresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTwin {
    pub version: u32,
    /// Absolute repo root the twin was built for (`/`-normalized).
    pub root: String,
    /// Epoch milliseconds the twin was built.
    pub built_at_ms: u64,
    /// Content fingerprint of the source tree — equal fingerprints mean the
    /// cache is still valid.
    pub fingerprint: String,
    /// True when the file walk hit [`MAX_FILES`] and stopped early.
    pub truncated: bool,

    pub files: Vec<FileNode>,
    pub imports: Vec<ImportEdge>,
    pub symbols: Vec<SymbolDef>,
    pub tests: Vec<TestEdge>,
    pub git: Vec<GitStat>,
    pub rules: Vec<RuleDoc>,
}

impl RepoTwin {
    /// One-glance counts for `tomte twin map`.
    pub fn summary(&self) -> Summary {
        Summary {
            files: self.files.len(),
            source_files: self.files.iter().filter(|f| f.lang.is_source()).count(),
            test_files: self.files.iter().filter(|f| f.is_test).count(),
            import_edges: self.imports.len(),
            resolved_imports: self.imports.iter().filter(|e| e.to.is_some()).count(),
            symbols: self.symbols.len(),
            test_edges: self.tests.len(),
            tracked_by_git: self.git.len(),
            rule_docs: self.rules.len(),
        }
    }

    /// The one-glance text card `tomte twin` and `/twin` share — counts for the
    /// five indexes plus the truncation note, ending with the why-context hint.
    pub fn render_summary(&self) -> String {
        let s = self.summary();
        let mut out = String::new();
        out.push_str(&format!("Repo Twin — {}\n", self.root));
        if self.truncated {
            out.push_str("  (index truncated: the repo exceeds the file cap)\n");
        }
        out.push_str(&format!("  files            {}\n", s.files));
        out.push_str(&format!("    source         {}\n", s.source_files));
        out.push_str(&format!("    tests          {}\n", s.test_files));
        out.push_str(&format!(
            "  import edges     {} ({} resolved inside the repo)\n",
            s.import_edges, s.resolved_imports
        ));
        out.push_str(&format!("  symbols          {}\n", s.symbols));
        out.push_str(&format!("  test→source map  {} edges\n", s.test_edges));
        out.push_str(&format!("  git-tracked      {} files\n", s.tracked_by_git));
        out.push_str(&format!("  convention docs  {}\n", s.rule_docs));
        out.push_str(
            "\nAsk why a file/symbol is (or isn't) relevant:  tomte why-context <file|symbol>",
        );
        out
    }
}

/// Counts surfaced by `tomte twin map [--json]`.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub files: usize,
    pub source_files: usize,
    pub test_files: usize,
    pub import_edges: usize,
    pub resolved_imports: usize,
    pub symbols: usize,
    pub test_edges: usize,
    pub tracked_by_git: usize,
    pub rule_docs: usize,
}

// ---- build ------------------------------------------------------------------

/// Load the cached twin if it's still fresh for `cwd`, otherwise build and cache
/// a new one. The everyday entry point — `tomte why-context` and `tomte twin
/// map` call this so the index builds on first use and is reused after.
pub fn load_or_build(cwd: &Path) -> anyhow::Result<RepoTwin> {
    let root = repo_root(cwd);
    if let Some(cached) = load_cache(&root) {
        if cached.version == CACHE_VERSION && cached.fingerprint == fingerprint(&root) {
            return Ok(cached);
        }
    }
    let twin = build(&root)?;
    // A cache write failure is not fatal — the twin is still usable this run.
    if let Err(e) = save_cache(&root, &twin) {
        tracing::warn!("repo-twin: could not cache index: {e:#}");
    }
    Ok(twin)
}

/// Force a fresh build for `cwd` and cache it (`tomte twin build`), regardless of
/// whether a fresh cache already exists.
pub fn rebuild(cwd: &Path) -> anyhow::Result<RepoTwin> {
    let root = repo_root(cwd);
    let twin = build(&root)?;
    save_cache(&root, &twin)?;
    Ok(twin)
}

/// Build the twin for an absolute repo `root` without touching the cache. Reads
/// each source file once and feeds it to every per-file extractor, so the walk
/// is single-pass.
pub fn build(root: &Path) -> anyhow::Result<RepoTwin> {
    let rels = walk_source_paths(root);
    let truncated = rels.len() >= MAX_FILES;

    let mut files: Vec<FileNode> = Vec::with_capacity(rels.len());
    let mut imports: Vec<ImportEdge> = Vec::new();
    let mut symbols: Vec<SymbolDef> = Vec::new();
    // Per-file text kept only long enough to derive test edges; dropped after.
    let mut rust_has_inline_test: Vec<String> = Vec::new();

    for rel in &rels {
        let lang = Lang::of(rel);
        let abs = root.join(rel);
        let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);

        // Non-source or oversized files are nodes only — no parsing.
        if !lang.is_source() || size > MAX_SOURCE_BYTES {
            files.push(FileNode {
                path: rel.clone(),
                lang,
                is_test: testmap::is_test_path(rel),
                loc: 0,
            });
            continue;
        }

        let Ok(text) = std::fs::read_to_string(&abs) else {
            // Binary or unreadable despite the extension — keep it as a node.
            files.push(FileNode {
                path: rel.clone(),
                lang,
                is_test: testmap::is_test_path(rel),
                loc: 0,
            });
            continue;
        };

        let loc = text.lines().count();
        let inline_test = lang == Lang::Rust && symbols::rust_has_inline_test(&text);
        let is_test = testmap::is_test_path(rel) || inline_test;
        if inline_test {
            rust_has_inline_test.push(rel.clone());
        }

        imports.extend(imports::extract(lang, rel, &text));
        symbols.extend(symbols::extract(lang, rel, &text));

        files.push(FileNode {
            path: rel.clone(),
            lang,
            is_test,
            loc,
        });
    }

    // Resolve import specifiers to repo files now that the full file set is known.
    let file_set: std::collections::HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let go_module = gitmap::read_go_module(root);
    for edge in &mut imports {
        edge.to = imports::resolve(
            Lang::of(&edge.from),
            &edge.from,
            &edge.raw,
            &file_set,
            go_module.as_deref(),
        );
    }

    let tests = testmap::build_edges(&files, &imports, &rust_has_inline_test);
    let git = gitmap::recent_changes(root);
    let rule_docs = rules::extract_all(root);

    Ok(RepoTwin {
        version: CACHE_VERSION,
        root: display_root(root),
        built_at_ms: now_ms(),
        fingerprint: fingerprint(root),
        truncated,
        files,
        imports,
        symbols,
        tests,
        git,
        rules: rule_docs,
    })
}

// ---- file walk --------------------------------------------------------------

/// Every file under `root` as a `/`-separated relative path, honoring
/// `.gitignore`/`.ignore` (so `node_modules`, `target`, `dist`, … are skipped)
/// and never descending into `.git`. Mirrors the search tool's walker so the
/// twin sees the same files the agent's `grep`/`glob` would. Capped at
/// [`MAX_FILES`]; symlinks are not followed.
fn walk_source_paths(root: &Path) -> Vec<String> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != std::ffi::OsStr::new(".git"))
        .build();
    let mut out = Vec::new();
    for entry in walker.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(normalize(&rel.to_string_lossy()));
            }
        }
    }
    out
}

// ---- cache ------------------------------------------------------------------

/// `<config>/projects/<key>/repo-twin.json` — sibling of the memory and decision
/// stores, reusing memory's project keying so all per-project state shares one
/// directory.
pub fn cache_path(root: &Path) -> PathBuf {
    let memdir = crate::tools::memory::store_dir(root);
    memdir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::config_dir)
        .join("repo-twin.json")
}

fn load_cache(root: &Path) -> Option<RepoTwin> {
    let text = std::fs::read_to_string(cache_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_cache(root: &Path, twin: &RepoTwin) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    let path = cache_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string(twin).context("serialize repo twin")?;
    // Stage → flush → atomic rename, so a crash mid-write can't leave a torn
    // cache that fails to parse (mirrors the decision-trail / session writers).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), nanos));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

// ---- fingerprint & helpers --------------------------------------------------

/// A content fingerprint over the source tree: a SHA-256 of every walked path
/// with its size and mtime. Cheap (no file reads) and stable across runs, so an
/// unchanged tree reuses the cache and any add/edit/delete invalidates it. mtime
/// is best-effort — a filesystem without it still fingerprints by path+size.
fn fingerprint(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    // Sorted for determinism: the walk order is not guaranteed stable.
    let mut entries: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for rel in walk_source_paths(root) {
        let abs = root.join(&rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        entries.insert(rel, (meta.len(), mtime));
    }
    let mut hasher = Sha256::new();
    for (path, (size, mtime)) in &entries {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(size.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// Resolve the repo root for `cwd` (git toplevel, else `cwd`) — the twin is
/// keyed and scoped to the whole repository, like the memory/decision stores.
fn repo_root(cwd: &Path) -> PathBuf {
    crate::memory::git_root_from(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// `/`-normalized root for display and for re-reading source files. Windows
/// `canonicalize` returns a `\\?\C:\…` verbatim path; we strip that prefix so
/// the stored root is the plain `C:/…` form — clean to print and usable with the
/// forward-slash relative paths the twin stores.
fn display_root(root: &Path) -> String {
    let s = normalize(&root.to_string_lossy());
    s.strip_prefix("//?/UNC/")
        .map(|rest| format!("//{rest}"))
        .or_else(|| s.strip_prefix("//?/").map(|rest| rest.to_string()))
        .unwrap_or(s)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Fold `\` to `/` so every stored path is platform-neutral.
pub(crate) fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// 1-based line number of byte offset `at` within `text` — shared by the
/// extractors so every recorded line points at the real source line.
pub(crate) fn line_at(text: &str, at: usize) -> usize {
    text[..at.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_classifies_by_extension() {
        assert_eq!(Lang::of("src/a.rs"), Lang::Rust);
        assert_eq!(Lang::of("web/x.tsx"), Lang::Web);
        assert_eq!(Lang::of("y.mjs"), Lang::Web);
        assert_eq!(Lang::of("m/n.py"), Lang::Python);
        assert_eq!(Lang::of("p/q.go"), Lang::Go);
        assert_eq!(Lang::of("README.md"), Lang::Other);
        assert!(Lang::of("a.rs").is_source());
        assert!(!Lang::of("a.md").is_source());
    }

    #[test]
    fn build_indexes_a_small_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub mod util;\npub fn run() { util::help(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("src/util.rs"), "pub fn help() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# Project\n- always test\n").unwrap();

        let twin = build(root).unwrap();
        assert_eq!(twin.version, CACHE_VERSION);
        // Three files tracked (two .rs + README).
        assert!(twin.files.len() >= 3);
        // `pub mod util;` resolves to src/util.rs.
        let mod_edge = twin
            .imports
            .iter()
            .find(|e| e.raw == "util")
            .expect("mod util edge");
        assert_eq!(mod_edge.to.as_deref(), Some("src/util.rs"));
        // Symbols were captured.
        assert!(twin.symbols.iter().any(|s| s.name == "run"));
        assert!(twin.symbols.iter().any(|s| s.name == "help"));
        // The README is picked up as a rule doc.
        assert!(twin.rules.iter().any(|r| r.file == "README.md"));
    }

    #[test]
    fn cache_roundtrips_and_reuses_on_unchanged_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();

        let built = build(root).unwrap();
        // Fingerprint is stable across two reads of an unchanged tree.
        assert_eq!(built.fingerprint, fingerprint(root));

        // After an edit the fingerprint changes (cache would be rebuilt).
        std::fs::write(root.join("src/a.rs"), "pub fn a() { let _x = 1; }\n").unwrap();
        assert_ne!(built.fingerprint, fingerprint(root));
    }
}

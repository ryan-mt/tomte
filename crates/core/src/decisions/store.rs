use super::*;

/// `<config>/projects/<key>/decisions.jsonl` — sibling of the memory store,
/// reusing memory's project keying so both share one per-project directory.
pub fn store_path(cwd: &Path) -> PathBuf {
    let memdir = crate::tools::memory::store_dir(cwd);
    memdir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::config_dir)
        .join("decisions.jsonl")
}

/// Append one decision to the trail, creating the store directory if needed.
pub fn append(cwd: &Path, record: &DecisionRecord) -> anyhow::Result<()> {
    append_at(&store_path(cwd), record)
}

pub(crate) fn append_at(path: &Path, record: &DecisionRecord) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(record).context("serialize decision")?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    // Write the record and its newline in ONE call: under POSIX `O_APPEND` a
    // single small write lands atomically, so two tomte sessions appending to the
    // same project trail can't interleave a half-line — a `writeln!` lowers to
    // multiple writes and could. `sync_all` then flushes it so a crash right after
    // we report a decision "recorded" can't drop it (the durability bar the
    // session/config writers already hold); the dir fsync covers a freshly-created
    // trail file's directory entry.
    f.write_all(format!("{line}\n").as_bytes())
        .with_context(|| format!("append {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("flush {}", path.display()))?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent);
    }
    Ok(())
}

/// Best-effort directory fsync so a preceding append/rename is durable across a
/// crash. A no-op where directory fsync isn't supported (e.g. Windows std).
pub(super) fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Load the whole trail (oldest first). Malformed lines are skipped, not fatal,
/// so one bad hand-edit can't sink the rest of the trail.
pub fn load(cwd: &Path) -> Vec<DecisionRecord> {
    load_at(&store_path(cwd))
}

pub(crate) fn load_at(path: &Path) -> Vec<DecisionRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DecisionRecord>(l).ok())
        .collect()
}

/// Decisions recorded at a given location, in record order (oldest first).
pub fn for_loc(cwd: &Path, loc: &str) -> Vec<DecisionRecord> {
    let needle = loc.trim();
    load(cwd).into_iter().filter(|d| d.loc == needle).collect()
}

/// Like [`for_loc`], but first heals drifted lines in memory against the working
/// tree (without persisting) — so `tomte why <file:line>` finds a decision even
/// after the code shifted since it was recorded, matching the drift tolerance
/// the injected trail already gets. A read-only query never mutates the store.
pub fn for_loc_live(cwd: &Path, loc: &str) -> Vec<DecisionRecord> {
    for_loc_live_at(&store_path(cwd), cwd, loc)
}

pub(crate) fn for_loc_live_at(store: &Path, root: &Path, loc: &str) -> Vec<DecisionRecord> {
    let needle = loc.trim();
    let mut records = load_at(store);
    let _ = heal_locs(&mut records, root);
    records.into_iter().filter(|d| d.loc == needle).collect()
}

/// Decisions recorded anywhere in a given file, in record order (oldest first).
/// Unlike `for_loc` (which pins to an exact `file:line`), this matches on the
/// *file* component of each `loc`, so it returns every decision in the file
/// regardless of line. The query may be a bare path or a `file:line` — the line
/// suffix is ignored — and `\` is normalized to `/` so a Windows-style query
/// matches the forward-slash `loc`s the agent records. Powers `tomte blame
/// <file>`, and is the file-scoped lookup the conscience lane (A2) reuses.
pub fn for_file(cwd: &Path, file: &str) -> Vec<DecisionRecord> {
    filter_for_file(load(cwd), file)
}

/// The pure filter behind `for_file`, over an already-loaded trail so the
/// matching logic is testable without touching the real store.
pub(super) fn filter_for_file(records: Vec<DecisionRecord>, file: &str) -> Vec<DecisionRecord> {
    let needle = normalize_file(parse_loc(file.trim()).0);
    records
        .into_iter()
        .filter(|d| normalize_file(parse_loc(&d.loc).0) == needle)
        .collect()
}

/// Render the recorded decisions for `file` as short "house rules" lines for the
/// Pillar-1 pre-flight: up to 3, most-recent-first, each `<decision> — <why>
/// (<model>)`, plus a `+k more · tomte why <file>` overflow line when there are
/// more. Empty when the file has no recorded decisions. Pillar 5 (A2 Tier 1):
/// pure surfacing at the instant of an edit — recall at the moment of risk, not
/// detection, so it can never be wrong.
pub fn house_rules(cwd: &Path, file: &str) -> Vec<String> {
    house_rules_from(for_file(cwd, file), file)
}

/// The pure renderer behind [`house_rules`], over an already-loaded set so the
/// cap/overflow logic is testable without touching the real store.
pub(super) fn house_rules_from(records: Vec<DecisionRecord>, file: &str) -> Vec<String> {
    const MAX: usize = 3;
    if records.is_empty() {
        return Vec::new();
    }
    let total = records.len();
    let mut out: Vec<String> = records
        .iter()
        .rev()
        .take(MAX)
        .map(|d| {
            format!(
                "{} — {} ({})",
                gist(&d.decision, 48),
                gist(&d.why, 48),
                d.model
            )
        })
        .collect();
    if total > MAX {
        out.push(format!(
            "+{} more · tomte why {}",
            total - MAX,
            normalize_file(parse_loc(file).0)
        ));
    }
    out
}

/// Normalize a file path for trail matching: trim and fold `\` to `/`, so a
/// query typed with Windows separators lines up with the forward-slash `loc`s.
pub(crate) fn normalize_file(file: &str) -> String {
    file.trim().replace('\\', "/")
}

// ---- Drift Watch: reconcile the trail against the working tree (Pillar 5) ---

/// Split a `loc` into its file part and an optional 1-based line number. A
/// trailing `:<digits>` is the line; anything else is a file-only location.
/// `src/a.rs:88` -> (`src/a.rs`, Some(88)); `src/a.rs` -> (`src/a.rs`, None).
pub(crate) fn parse_loc(loc: &str) -> (&str, Option<usize>) {
    match loc.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            match line.parse::<usize>() {
                Ok(n) if n >= 1 => (file, Some(n)),
                _ => (loc, None),
            }
        }
        _ => (loc, None),
    }
}

/// Snapshot the trimmed source line at `loc` for use as a drift anchor. Returns
/// `None` for a file-only `loc`, a missing file, an out-of-range line, or a
/// blank line (a blank anchor would match everywhere and is useless).
pub fn capture_anchor(cwd: &Path, loc: &str) -> Option<String> {
    let (file, line) = parse_loc(loc.trim());
    let n = line?;
    let text = std::fs::read_to_string(cwd.join(file)).ok()?;
    let raw = text.lines().nth(n - 1)?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

// ---- auto-capture: parse a self-check answer into a decision (Pillar 2) ------
// After a turn that changed files, the agent asks the active model whether it
// made a non-obvious decision worth keeping (provider-agnostic — see
// `Agent::maybe_capture_decision`). The model's reply is parsed here, so the
// trail populates itself without the model having to call `record_decision`.

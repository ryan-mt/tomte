use super::*;

/// What `reconcile` found, for the `tomte why --reconcile` summary. Derives
/// `Serialize` so `tomte why --reconcile --json` can emit it for a CI drift-gate.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    /// Records whose anchored line is still where the `loc` says — left as is.
    pub present: usize,
    /// Records with no anchor or a file-only `loc` — nothing to reconcile.
    pub skipped: usize,
    /// `(old_loc, new_loc)` for records whose line drifted and self-healed.
    pub moved: Vec<(String, String)>,
    /// `loc`s whose anchored line is gone from the file entirely.
    pub gone: Vec<String>,
    /// `loc`s whose anchored line now appears in 2+ places — can't auto-heal.
    pub ambiguous: Vec<String>,
}

impl ReconcileReport {
    /// True when at least one record drifted and a rewrite is warranted.
    pub fn changed(&self) -> bool {
        !self.moved.is_empty()
    }
    /// `loc`s that need a human's eyes: gone or ambiguous.
    pub fn stale(&self) -> usize {
        self.gone.len() + self.ambiguous.len()
    }
}

/// Reconcile every anchored decision against the current working tree: heal the
/// `loc` of any whose line merely moved, and flag any whose line is gone or
/// ambiguous. Records that moved are persisted (atomic rewrite of the trail);
/// nothing else is touched. Records with no anchor (older format) or a file-only
/// `loc` are left exactly as they are. The fix for `for_loc`'s exact-line match
/// going stale the moment code shifts. Pillar 5 — Drift Watch (A1).
pub fn reconcile(cwd: &Path) -> ReconcileReport {
    reconcile_at(&store_path(cwd), cwd)
}

/// `reconcile` with the store path and the source-file root passed separately,
/// so tests can drive the logic without touching the real config directory.
pub(crate) fn reconcile_at(store: &Path, root: &Path) -> ReconcileReport {
    let mut records = load_at(store);
    let report = heal_locs(&mut records, root);
    if report.changed() {
        // Re-load and re-heal immediately before the rewrite. `heal_locs` scans the
        // whole working tree, so the gap between the first `load_at` and the rename
        // is wide; a decision another tomte session appends in that gap would be
        // clobbered by our stale snapshot (the trail is shared per-project, and the
        // moat must not silently lose an entry). Re-reading just before persisting
        // carries any such append into the rewrite, shrinking the lost-append
        // window to load→rename; `heal_locs` is deterministic, so re-healing the
        // fresh set yields the same result. (A fully airtight fix needs a
        // cross-process lock; this closes the realistic case without adding one.)
        let mut fresh = load_at(store);
        heal_locs(&mut fresh, root);
        if let Err(e) = save_all(store, &fresh) {
            tracing::warn!("decision-trail reconcile could not persist healed locs: {e:#}");
        }
    }
    report
}

/// Heal the in-memory `loc` of every anchored record whose line merely moved, and
/// tally what is present / moved / gone / ambiguous / skipped. Reads the working
/// tree but never persists — the I/O-free core shared by `reconcile_at` (which
/// then saves) and the read-only `*_live` query paths (which heal only so a
/// lookup matches the current line, without mutating the store).
pub(super) fn heal_locs(records: &mut [DecisionRecord], root: &Path) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    for rec in records.iter_mut() {
        let Some(anchor) = rec.anchor.clone() else {
            report.skipped += 1;
            continue;
        };
        let (file, line) = parse_loc(&rec.loc);
        let Some(n) = line else {
            report.skipped += 1;
            continue;
        };
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            report.gone.push(rec.loc.clone());
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        if lines.get(n - 1).map(|l| l.trim()) == Some(anchor.as_str()) {
            report.present += 1;
            continue;
        }
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim() == anchor)
            .map(|(i, _)| i + 1)
            .collect();
        match hits.as_slice() {
            [only] => {
                let new_loc = format!("{file}:{only}");
                report.moved.push((rec.loc.clone(), new_loc.clone()));
                rec.loc = new_loc;
            }
            [] => report.gone.push(rec.loc.clone()),
            _ => report.ambiguous.push(rec.loc.clone()),
        }
    }
    report
}

/// Atomically rewrite the whole trail (used by `reconcile` after a heal): write
/// to a sibling temp file, then rename over the store so a crash can't leave a
/// half-written trail. Malformed lines that `load_at` skipped are not preserved
/// — a reconcile normalizes the file.
pub(super) fn save_all(path: &Path, records: &[DecisionRecord]) -> anyhow::Result<()> {
    use anyhow::Context as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut body = String::new();
    for r in records {
        body.push_str(&serde_json::to_string(r).context("serialize decision")?);
        body.push('\n');
    }
    // Unique per-process temp name so two concurrent reconciles can't clobber
    // each other's staging file before the rename (mirrors session/config saves).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("jsonl.tmp.{}.{}", std::process::id(), nanos));
    // Stage → flush → atomic rename → flush the directory, so a crash leaves either
    // the old trail or the whole new one, never a torn file (mirrors the session
    // writer). `std::fs::write` alone left the staged bytes unflushed before the
    // rename, so a crash could publish an empty/partial trail.
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("flush {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent);
    }
    Ok(())
}

// ---- CLI rendering (`tomte why`) -------------------------------------------

//! `tomte seal` — notarize the Proof Capsule onto the commit itself.
//!
//! `tomte prove` answers for the tree right now; the seal makes that answer
//! permanent: collected at a clean HEAD, attached as a git note under
//! `refs/notes/tomte-seal`, and bound to the commit + tree ids — so the proof
//! is pushed/fetched with the history it certifies. `seal show` reads a
//! commit's seal back; `seal verify` is the CI gate — exit 0 only when the
//! commit is sealed AND the sealed capsule is green. Headless by design, like
//! `rounds`: sealing wants a clean tree, which is a commit-time posture, not a
//! mid-session one.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use tomte_core::seal::{self, ReadError};

#[derive(Debug, Subcommand)]
pub enum SealAction {
    /// Print the seal recorded on a commit (defaults to HEAD).
    Show {
        /// Commit to read — any git revision spelling. Defaults to HEAD.
        rev: Option<String>,
        /// Emit the seal as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Gate on a commit's seal: exit 0 only when the commit is sealed, the
    /// seal is bound to this exact commit and tree, and the sealed capsule is
    /// green (at least one check ran, none failed).
    Verify {
        /// Commit to gate on — any git revision spelling. Defaults to HEAD.
        rev: Option<String>,
        /// Emit the verdict as JSON instead of the one-line text.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
}

pub async fn run(
    action: Option<SealAction>,
    json: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    match action {
        None => create(json, cwd).await,
        Some(SealAction::Show { rev, json, cwd }) => show(rev, json, cwd).await,
        Some(SealAction::Verify { rev, json, cwd }) => verify(rev, json, cwd).await,
    }
}

fn enter(cwd: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    Ok(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

/// Bare `tomte seal`: collect the capsule at a clean HEAD and notarize it.
/// Mirrors `tomte prove`'s exit semantics — a failed check makes the run
/// non-zero (the seal is still written; it records what was observed).
async fn create(json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    let here = enter(cwd)?;
    let sealed = match seal::create(&here).await {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&sealed)?);
    } else {
        let subject = seal::commit_subject(&here, &sealed.commit).await;
        println!("{}", seal::render(&sealed, subject.as_deref()));
    }
    if !sealed.capsule.verified() {
        std::process::exit(1);
    }
    Ok(())
}

async fn show(rev: Option<String>, json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    let here = enter(cwd)?;
    let rev = rev.unwrap_or_else(|| "HEAD".to_string());
    let found = match seal::read(&here, &rev).await {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{}", read_error_text(&e));
            std::process::exit(1);
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&found.seal)?);
        return Ok(());
    }
    let subject = seal::commit_subject(&here, &found.commit).await;
    print!("{}", seal::render(&found.seal, subject.as_deref()));
    // Showing is reading — a suspect binding is surfaced, not hidden, and the
    // exit code stays 0 (gate with `seal verify`).
    if found.seal.commit != found.commit {
        println!(
            "\n⚠️ this note was written for commit {}, not {} — it does not answer for this commit",
            seal::short(&found.seal.commit),
            seal::short(&found.commit)
        );
    }
    Ok(())
}

async fn verify(rev: Option<String>, json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    let here = enter(cwd)?;
    let rev = rev.unwrap_or_else(|| "HEAD".to_string());
    let (commit, sealed, reason) = match seal::read(&here, &rev).await {
        Ok(found) => {
            let reason = seal::verify_failure(&found.seal, &found.commit, &found.tree);
            (Some(found.commit), true, reason)
        }
        Err(ReadError::NoSeal { commit }) => (
            Some(commit),
            false,
            Some("the commit carries no seal".to_string()),
        ),
        Err(ReadError::Unreadable { commit, error }) => (
            Some(commit),
            true,
            Some(format!("the seal note is unreadable ({error})")),
        ),
        Err(ReadError::Git(msg)) => (None, false, Some(msg)),
    };

    let verified = reason.is_none();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "rev": rev,
                "commit": commit,
                "sealed": sealed,
                "verified": verified,
                "reason": reason,
            }))?
        );
    } else {
        let name = commit.as_deref().map(seal::short).unwrap_or(rev.as_str());
        match &reason {
            None => println!("✅ {name} is sealed and verified"),
            Some(why) => println!("❌ {name} does not verify — {why}"),
        }
    }
    if !verified {
        std::process::exit(1);
    }
    Ok(())
}

fn read_error_text(e: &ReadError) -> String {
    match e {
        ReadError::Git(msg) => format!("seal: {msg}"),
        ReadError::NoSeal { commit } => format!(
            "no seal on {} — `tomte seal` writes one at that commit",
            seal::short(commit)
        ),
        ReadError::Unreadable { commit, error } => format!(
            "the seal note on {} is unreadable ({error})",
            seal::short(commit)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_error_text_names_the_miss_and_the_fix() {
        let no_seal = read_error_text(&ReadError::NoSeal {
            commit: "aaaa1111ffff0000".into(),
        });
        assert!(no_seal.contains("no seal on aaaa1111"));
        assert!(no_seal.contains("tomte seal"), "points at the fix");

        let unreadable = read_error_text(&ReadError::Unreadable {
            commit: "aaaa1111ffff0000".into(),
            error: "expected value".into(),
        });
        assert!(unreadable.contains("unreadable"));
        assert!(unreadable.contains("expected value"));

        let git = read_error_text(&ReadError::Git("bad revision".into()));
        assert!(git.contains("bad revision"));
    }
}

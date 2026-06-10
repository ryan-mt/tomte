//! `tomte update` — self-update from the project's GitHub releases.
//!
//! Mirrors the install path `action.yml` already trusts: resolve the latest
//! release tag, download the platform archive plus its published `.sha256`,
//! verify the checksum, extract with the system `tar` (bsdtar on Windows and
//! macOS, GNU tar on Linux — both auto-detect the compression), and swap the
//! running binary in place. The network is touched only for the release
//! lookup and the two asset downloads; nothing else is read or written
//! outside a temp dir and the binary's own directory.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

const REPO: &str = "ryan-mt/tomte";

pub async fn run(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("tomte v{current} — checking github.com/{REPO} for the latest release…");
    let tag = latest_release_tag().await?;
    let latest = tag.trim_start_matches('v');

    match (parse_version(current), parse_version(latest)) {
        (Some(cur), Some(rel)) if rel == cur => {
            println!("already up to date (v{current})");
            return Ok(());
        }
        (Some(cur), Some(rel)) if rel < cur => {
            // A dev build is ahead of the newest release; installing would be a
            // silent downgrade (and on a dev machine would clobber the build
            // the shim points at).
            println!(
                "this build (v{current}) is newer than the latest release ({tag}) — nothing to do"
            );
            return Ok(());
        }
        _ => {}
    }

    println!("update available: v{current} → {tag}");
    if check_only {
        println!("run `tomte update` to install it");
        return Ok(());
    }

    let Some(asset) = asset_for_platform() else {
        bail!(
            "no published binary for this platform ({}-{}); build from source instead",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    };
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    eprintln!("downloading {asset}…");
    let archive_bytes = fetch(&format!("{base}/{asset}")).await?;
    let sum_text = String::from_utf8(fetch(&format!("{base}/{asset}.sha256")).await?)
        .context("read the published .sha256 file")?;
    let want = parse_sha256(&sum_text)
        .ok_or_else(|| anyhow!("the published {asset}.sha256 did not contain a SHA-256 hash"))?;
    let got = sha256_hex(&archive_bytes);
    if want != got {
        bail!("checksum mismatch for {asset} (want {want}, got {got}) — refusing to install");
    }
    eprintln!("checksum verified ({})", &got[..12]);

    let tmp = tempfile::tempdir().context("create a temp dir for the download")?;
    let archive_path = tmp.path().join(asset);
    std::fs::write(&archive_path, &archive_bytes)
        .with_context(|| format!("write {}", archive_path.display()))?;
    extract(&archive_path, tmp.path()).await?;

    let folder = asset.trim_end_matches(".tar.gz").trim_end_matches(".zip");
    let bin_name = if cfg!(windows) { "tomte.exe" } else { "tomte" };
    let new_bin = tmp.path().join(folder).join(bin_name);
    if !new_bin.is_file() {
        bail!("extracted archive did not contain {folder}/{bin_name}");
    }

    let exe = std::env::current_exe().context("locate the running binary")?;
    println!("installing {tag} over {}", exe.display());
    replace_binary(&new_bin, &exe)?;
    println!("✅ updated to {tag} — restart tomte to use it");
    Ok(())
}

/// Resolve the latest release tag without the rate-limited REST API: GitHub
/// redirects `/releases/latest` to `/releases/tag/<tag>`, so the tag is in the
/// final URL after redirects.
async fn latest_release_tag() -> Result<String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    let resp = client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    parse_tag_from_url(resp.url().as_str())
        .ok_or_else(|| anyhow!("no releases found at github.com/{REPO}"))
}

async fn fetch(url: &str) -> Result<Vec<u8>> {
    let resp = client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    Ok(resp
        .bytes()
        .await
        .with_context(|| format!("GET {url}"))?
        .to_vec())
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("tomte-update/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

/// Extract `archive` into `dest` with the system `tar`. One tool covers every
/// platform we publish for: bsdtar (Windows 10+, macOS) extracts both .tar.gz
/// and .zip with `-xf`, and GNU tar (Linux) auto-detects the gzip compression.
async fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let out = tokio::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .await
        .context("run `tar` (ships with Windows 10+, macOS, and Linux)")?;
    if !out.status.success() {
        bail!(
            "tar failed to extract {}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Swap `exe` for `new_bin`. The staged copy lands in the binary's own
/// directory so the final rename never crosses a filesystem. On Windows the
/// running exe can't be overwritten but CAN be renamed away, so it moves to
/// `tomte.exe.old` first (a stale one from the previous update is cleaned up,
/// best-effort) and the rename is rolled back if the install step fails.
pub(crate) fn replace_binary(new_bin: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("binary path {} has no parent directory", exe.display()))?;
    let staged = dir.join(format!(".tomte-update-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::copy(new_bin, &staged) {
        return Err(e).with_context(|| {
            format!(
                "stage the new binary at {} (is the directory writable?)",
                staged.display()
            )
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&staged);
            return Err(e).context("mark the new binary executable");
        }
    }
    let install = || -> Result<()> {
        #[cfg(windows)]
        {
            let old = exe.with_extension("exe.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(exe, &old).context("move the running binary aside")?;
            if let Err(e) = std::fs::rename(&staged, exe) {
                let _ = std::fs::rename(&old, exe);
                return Err(e).context("install the new binary");
            }
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&staged, exe).context("install the new binary")?;
        }
        Ok(())
    };
    let result = install();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// The release asset for this OS/arch — must match `release.yml`'s matrix
/// (and `action.yml`'s install table) exactly.
fn asset_for_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("tomte-x86_64-pc-windows-msvc.zip"),
        ("linux", "x86_64") => Some("tomte-x86_64-unknown-linux-gnu.tar.gz"),
        ("macos", "x86_64") => Some("tomte-x86_64-apple-darwin.tar.gz"),
        ("macos", "aarch64") => Some("tomte-aarch64-apple-darwin.tar.gz"),
        _ => None,
    }
}

/// Pull `<tag>` out of a `…/releases/tag/<tag>` URL. A repo with no releases
/// redirects to the releases listing instead, which has no `/tag/` segment.
fn parse_tag_from_url(url: &str) -> Option<String> {
    let (_, tail) = url.split_once("/releases/tag/")?;
    let tag = tail.split(['?', '#', '/']).next().unwrap_or("");
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

/// First token of a `sha256sum`-style file (`<hash>  <name>`), lowercased.
/// Tolerates a BOM and either hash case; rejects anything that isn't 64 hex
/// chars so a proxy error page can't pass as a checksum.
fn parse_sha256(text: &str) -> Option<String> {
    let token = text
        .trim_start_matches('\u{feff}')
        .split_whitespace()
        .next()?
        .to_ascii_lowercase();
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(token)
    } else {
        None
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `x.y.z` → comparable triple; a pre-release suffix (`0.1.0-beta.2`) compares
/// by its numeric part only.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_parses_from_a_release_redirect_url() {
        assert_eq!(
            parse_tag_from_url("https://github.com/ryan-mt/tomte/releases/tag/v0.0.4").as_deref(),
            Some("v0.0.4")
        );
        // No releases → redirect lands on the listing page, not a tag.
        assert_eq!(
            parse_tag_from_url("https://github.com/ryan-mt/tomte/releases"),
            None
        );
        assert_eq!(parse_tag_from_url(""), None);
    }

    #[test]
    fn sha256_file_parses_hash_and_rejects_garbage() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_sha256(&format!("{hash}  tomte-x.zip\n")).as_deref(),
            Some(hash.as_str())
        );
        // Uppercase and a BOM (a re-encoded checksum file) still parse.
        let upper = format!("\u{feff}{}  tomte-x.zip", "ABCDEF0123456789".repeat(4));
        assert_eq!(
            parse_sha256(&upper).as_deref(),
            Some("abcdef0123456789".repeat(4).as_str())
        );
        // An HTML error page must not pass as a checksum.
        assert_eq!(parse_sha256("<html>Not Found</html>"), None);
        assert_eq!(parse_sha256(""), None);
    }

    #[test]
    fn version_triples_compare_numerically() {
        assert_eq!(parse_version("0.0.4"), Some((0, 0, 4)));
        assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
        assert!(parse_version("0.0.10").unwrap() > parse_version("0.0.9").unwrap());
        assert_eq!(parse_version("nonsense"), None);
    }

    #[test]
    fn platform_asset_matches_the_release_matrix() {
        // This build must map to one of release.yml's four published assets
        // (or None on a platform the release doesn't cover).
        if let Some(asset) = asset_for_platform() {
            assert!(matches!(
                asset,
                "tomte-x86_64-pc-windows-msvc.zip"
                    | "tomte-x86_64-unknown-linux-gnu.tar.gz"
                    | "tomte-x86_64-apple-darwin.tar.gz"
                    | "tomte-aarch64-apple-darwin.tar.gz"
            ));
            if cfg!(windows) {
                assert!(asset.ends_with(".zip"));
            } else {
                assert!(asset.ends_with(".tar.gz"));
            }
        }
    }

    #[test]
    fn replace_binary_swaps_the_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir
            .path()
            .join(if cfg!(windows) { "tomte.exe" } else { "tomte" });
        let new_bin = dir.path().join("downloaded");
        std::fs::write(&exe, b"old build").unwrap();
        std::fs::write(&new_bin, b"new build").unwrap();

        replace_binary(&new_bin, &exe).unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"new build");
        #[cfg(windows)]
        assert_eq!(
            std::fs::read(exe.with_extension("exe.old")).unwrap(),
            b"old build",
            "the running binary moves aside as .old on Windows"
        );
        // No staged temp file left behind.
        let stray: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tomte-update-"))
            .collect();
        assert!(stray.is_empty(), "staged copy must not be left behind");
    }
}

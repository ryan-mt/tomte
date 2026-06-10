use super::*;

pub(super) async fn execute_grep_with_commands(
    a: &GrepArgs,
    ctx: &ToolContext,
    rg_program: &str,
    grep_program: &str,
) -> Result<String> {
    let Some(mode) = normalize_grep_output_mode(a.output_mode.as_deref()) else {
        let mode = a.output_mode.as_deref().unwrap_or("<null>");
        return Err(anyhow::anyhow!(
            "output_mode must be 'content', 'files_with_matches', or 'count' (got '{mode}')"
        ));
    };
    let context_after = a.context_after.or(a.context);
    let context_before = a.context_before.or(a.context);

    let mut cmd = Command::new(rg_program);
    cmd.arg("--color=never");
    match mode {
        "files_with_matches" => {
            cmd.arg("--files-with-matches");
        }
        "count" => {
            cmd.arg("--count");
        }
        _ => {
            cmd.arg("--no-heading").arg("--line-number");
            if let Some(n) = context_after {
                cmd.arg("-A").arg(n.to_string());
            }
            if let Some(n) = context_before {
                cmd.arg("-B").arg(n.to_string());
            }
        }
    }
    if a.case_insensitive {
        cmd.arg("-i");
    }
    if a.multiline.unwrap_or(false) {
        cmd.arg("--multiline").arg("--multiline-dotall");
    }
    if let Some(g) = &a.glob {
        cmd.arg("--glob").arg(g);
    }
    if let Some(t) = &a.file_type {
        cmd.arg("--type").arg(t);
    }
    // `--` stops flag parsing so a pattern beginning with `-` (e.g. `-rf`)
    // is searched literally instead of being read as ripgrep flags. The
    // grep fallback below already does this.
    cmd.arg("--").arg(&a.pattern);
    if let Some(p) = &a.path {
        cmd.arg(resolved_relative_to_cwd(&ctx.cwd, p)?);
    } else {
        cmd.arg(".");
    }
    cmd.current_dir(&ctx.cwd);
    let out = run_capped(cmd, SEARCH_OUTPUT_CAP_BYTES).await;
    if let Ok((out, overran)) = out {
        // rg exits 0 on matches and 1 on "no matches" (both fine); exit 2+
        // is a real error (invalid regex, bad glob). Surface that instead of
        // returning empty stdout, which the model reads as "no matches".
        // When `overran`, we killed the child at the output cap ourselves, so
        // its non-success/signal status is expected — keep the capped matches.
        if !overran && !out.status.success() && out.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg = stderr.trim();
            return Err(anyhow::anyhow!(
                "ripgrep failed: {}",
                if msg.is_empty() { "unknown error" } else { msg }
            ));
        }
        let stdout = normalize_search_output_paths(&String::from_utf8_lossy(&out.stdout), mode);
        return Ok(apply_limits(&stdout, a.head_limit, a.offset, 8000));
    }
    // ripgrep could not be spawned. The external `grep` can't honor
    // `glob`/`file_type`/`multiline`, so when any is requested route straight to
    // the native engine (which now supports all three). Otherwise try external
    // `grep` first, falling back to native if that can't spawn either.
    if a.glob.is_some() || a.file_type.is_some() || a.multiline.unwrap_or(false) {
        return native_grep_search(a, ctx, mode, context_before, context_after);
    }

    let mut grep = Command::new(grep_program);
    grep.arg("-E").arg("-r");
    match mode {
        "files_with_matches" => {
            grep.arg("-l");
        }
        "count" => {
            grep.arg("-c");
        }
        _ => {
            grep.arg("-n");
            if let Some(n) = context_after {
                grep.arg("-A").arg(n.to_string());
            }
            if let Some(n) = context_before {
                grep.arg("-B").arg(n.to_string());
            }
        }
    }
    if a.case_insensitive {
        grep.arg("-i");
    }
    // `--` separates flags from positional args so a pattern starting
    // with `-` isn't misinterpreted as a flag.
    grep.arg("--").arg(&a.pattern);
    match a.path.as_deref() {
        Some(p) => grep.arg(resolved_relative_to_cwd(&ctx.cwd, p)?),
        None => grep.arg("."),
    };
    grep.current_dir(&ctx.cwd);
    let (out, overran) = match run_capped(grep, SEARCH_OUTPUT_CAP_BYTES).await {
        Ok(v) => v,
        // Neither ripgrep nor an external `grep` could be spawned (e.g. a stock
        // Windows box with no Unix tooling): fall back to a native, dependency-
        // free search instead of erroring out.
        Err(_) => return native_grep_search(a, ctx, mode, context_before, context_after),
    };
    if !overran && !out.status.success() && out.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr.trim();
        return Err(anyhow::anyhow!(
            "grep fallback failed: {}",
            if msg.is_empty() { "unknown error" } else { msg }
        ));
    }
    let stdout = normalize_search_output_paths(&String::from_utf8_lossy(&out.stdout), mode);
    let stdout = if mode == "count" {
        filter_zero_count_lines(&stdout)
    } else {
        stdout
    };
    Ok(apply_limits(&stdout, a.head_limit, a.offset, 8000))
}

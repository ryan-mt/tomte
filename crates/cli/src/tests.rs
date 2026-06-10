use super::*;
use clap::Parser;

// The broken-pipe classifier must catch the stdlib print-macro panic on
// every OS wording and nothing else — a real bug's panic must still abort.
#[test]
fn broken_pipe_panic_classifier_matches_only_pipe_aborts() {
    // Unix wording (os error 32) and the Windows wording (os error 232).
    assert!(is_broken_pipe_panic(
        "failed printing to stdout: Broken pipe (os error 32)"
    ));
    assert!(is_broken_pipe_panic(
        "failed printing to stdout: The pipe is being closed. (os error 232)"
    ));
    // eprintln!'s stderr variant.
    assert!(is_broken_pipe_panic(
        "failed printing to stderr: Broken pipe (os error 32)"
    ));
    // Real panics must NOT be swallowed.
    assert!(!is_broken_pipe_panic("index out of bounds: the len is 3"));
    assert!(!is_broken_pipe_panic(
        "called `Option::unwrap()` on a `None` value"
    ));
    // A different printing failure (disk full) is not a pipe close.
    assert!(!is_broken_pipe_panic(
        "failed printing to stdout: No space left on device (os error 28)"
    ));
}

#[test]
fn hidden_plan_mode_required_flag_parses_before_subcommand() {
    let cli =
        Cli::try_parse_from(["tomte", "--plan-mode-required", "chat", "inspect", "first"]).unwrap();

    assert!(cli.plan_mode_required);
    match cli.command {
        Some(Command::Chat { prompt, .. }) => {
            assert_eq!(prompt, vec!["inspect".to_string(), "first".to_string()]);
        }
        other => panic!("expected chat command, got {other:?}"),
    }
}

#[test]
fn hidden_plan_mode_required_flag_parses_after_subcommand() {
    let cli = Cli::try_parse_from(["tomte", "chat", "--plan-mode-required", "inspect"]).unwrap();

    assert!(cli.plan_mode_required);
}

#[test]
fn chat_prove_flag_parses_and_defaults_off() {
    let cli = Cli::try_parse_from(["tomte", "chat", "say ok", "--prove"]).unwrap();
    match cli.command {
        Some(Command::Chat { prove, .. }) => assert!(prove),
        other => panic!("expected chat command, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["tomte", "chat", "say ok"]).unwrap();
    match cli.command {
        Some(Command::Chat { prove, .. }) => assert!(!prove),
        other => panic!("expected chat command, got {other:?}"),
    }
}

#[test]
fn chat_continue_and_session_flags_parse_and_conflict() {
    let cli = Cli::try_parse_from(["tomte", "chat", "go on", "--continue"]).unwrap();
    match cli.command {
        Some(Command::Chat {
            continue_session,
            session,
            ..
        }) => {
            assert!(continue_session);
            assert!(session.is_none());
        }
        other => panic!("expected chat command, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["tomte", "chat", "go on", "--session", "abc123"]).unwrap();
    match cli.command {
        Some(Command::Chat {
            continue_session,
            session,
            ..
        }) => {
            assert!(!continue_session);
            assert_eq!(session.as_deref(), Some("abc123"));
        }
        other => panic!("expected chat command, got {other:?}"),
    }

    // Naming an exact session and asking for "the latest" at once is ambiguous.
    assert!(
        Cli::try_parse_from(["tomte", "chat", "go on", "--continue", "--session", "abc"]).is_err()
    );
}

#[test]
fn continue_flag_parses_with_no_subcommand() {
    let cli = Cli::try_parse_from(["tomte", "--continue"]).unwrap();
    assert!(cli.continue_session);
    assert!(cli.command.is_none());

    // Short form `-c`.
    let cli = Cli::try_parse_from(["tomte", "-c"]).unwrap();
    assert!(cli.continue_session);
    assert!(cli.command.is_none());

    // Off by default.
    let cli = Cli::try_parse_from(["tomte"]).unwrap();
    assert!(!cli.continue_session);
}

#[test]
fn run_alias_parses_as_chat_with_cwd_and_prompt_file() {
    let cli = Cli::try_parse_from([
        "tomte",
        "run",
        "--cwd",
        "/tmp/project",
        "--prompt-file",
        "task.md",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Chat {
            cwd, prompt_file, ..
        }) => {
            assert_eq!(cwd, Some(std::path::PathBuf::from("/tmp/project")));
            assert_eq!(prompt_file, Some(std::path::PathBuf::from("task.md")));
        }
        other => panic!("expected chat command via `run` alias, got {other:?}"),
    }
}

#[test]
fn dangerously_skip_permissions_defaults_off_and_parses_when_set() {
    let default_cli = Cli::try_parse_from(["tomte", "chat", "hi"]).unwrap();
    match default_cli.command {
        Some(Command::Chat {
            dangerously_skip_permissions,
            ..
        }) => assert!(
            !dangerously_skip_permissions,
            "unattended runs must default to the safe (gated) posture"
        ),
        other => panic!("expected chat command, got {other:?}"),
    }

    let opt_in = Cli::try_parse_from([
        "tomte",
        "run",
        "--dangerously-skip-permissions",
        "--cwd",
        "/tmp/p",
        "hi",
    ])
    .unwrap();
    match opt_in.command {
        Some(Command::Chat {
            dangerously_skip_permissions,
            ..
        }) => assert!(dangerously_skip_permissions),
        other => panic!("expected chat command, got {other:?}"),
    }
}

#[test]
fn json_chat_output_disables_stderr_tracing() {
    let cli = Cli::try_parse_from(["tomte", "chat", "--output-format", "json", "hi"]).unwrap();
    assert!(command_uses_json_output(&cli.command));

    let stream_cli =
        Cli::try_parse_from(["tomte", "chat", "--output-format", "stream-json", "hi"]).unwrap();
    assert!(command_uses_json_output(&stream_cli.command));

    let text_cli = Cli::try_parse_from(["tomte", "chat", "--output-format", "text", "hi"]).unwrap();
    assert!(!command_uses_json_output(&text_cli.command));
}

#[test]
fn blame_parses_file_and_cwd() {
    let cli = Cli::try_parse_from(["tomte", "blame", "--cwd", "/p", "src/auth.rs"]).unwrap();
    match cli.command {
        Some(Command::Blame { file, json, cwd }) => {
            assert_eq!(file, "src/auth.rs");
            assert!(!json, "blame defaults to rendered text, not JSON");
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected blame command, got {other:?}"),
    }
}

#[test]
fn why_json_flag_parses_and_defaults_off() {
    let on = Cli::try_parse_from(["tomte", "why", "--all", "--json"]).unwrap();
    match on.command {
        Some(Command::Why { all, json, .. }) => {
            assert!(all);
            assert!(json, "--json must enable JSON output");
        }
        other => panic!("expected why command, got {other:?}"),
    }

    let off = Cli::try_parse_from(["tomte", "why", "src/x.rs:1"]).unwrap();
    match off.command {
        Some(Command::Why { json, .. }) => assert!(!json, "why defaults to rendered text"),
        other => panic!("expected why command, got {other:?}"),
    }
}

#[test]
fn why_diff_parses_with_and_without_a_base() {
    let with_base = Cli::try_parse_from(["tomte", "why", "diff", "origin/main"]).unwrap();
    match with_base.command {
        Some(Command::Why { loc, base, .. }) => {
            assert_eq!(loc.as_deref(), Some("diff"));
            assert_eq!(base.as_deref(), Some("origin/main"));
        }
        other => panic!("expected why command, got {other:?}"),
    }

    let bare = Cli::try_parse_from(["tomte", "why", "diff"]).unwrap();
    match bare.command {
        Some(Command::Why { loc, base, .. }) => {
            assert_eq!(loc.as_deref(), Some("diff"));
            assert!(base.is_none(), "base defaults to auto-detection");
        }
        other => panic!("expected why command, got {other:?}"),
    }
}

#[test]
fn models_parses_with_and_without_json() {
    let cli = Cli::try_parse_from(["tomte", "models", "--json"]).unwrap();
    match cli.command {
        Some(Command::Models { json }) => assert!(json),
        other => panic!("expected models command, got {other:?}"),
    }
    let cli = Cli::try_parse_from(["tomte", "models"]).unwrap();
    match cli.command {
        Some(Command::Models { json }) => assert!(!json),
        other => panic!("expected models command, got {other:?}"),
    }
}

#[test]
fn blame_json_flag_parses() {
    let cli = Cli::try_parse_from(["tomte", "blame", "--json", "src/auth.rs"]).unwrap();
    match cli.command {
        Some(Command::Blame { json, .. }) => assert!(json),
        other => panic!("expected blame command, got {other:?}"),
    }
}

#[test]
fn rounds_flags_parse_and_default_to_proof_on() {
    let cli = Cli::try_parse_from([
        "tomte",
        "rounds",
        "--no-proof",
        "--json",
        "--out",
        "r.md",
        "--cwd",
        "/p",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Rounds {
            no_proof,
            json,
            out,
            cwd,
        }) => {
            assert!(no_proof);
            assert!(json);
            assert_eq!(out, Some(std::path::PathBuf::from("r.md")));
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected rounds command, got {other:?}"),
    }

    // Bare `tomte rounds` runs the proof pass — verification is the point.
    let bare = Cli::try_parse_from(["tomte", "rounds"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Rounds {
            no_proof: false,
            json: false,
            out: None,
            cwd: None
        })
    ));
}

#[test]
fn receipt_flags_parse_and_json_conflicts_with_html() {
    let cli = Cli::try_parse_from([
        "tomte",
        "receipt",
        "--html",
        "--session",
        "s-1",
        "--out",
        "RECEIPT.html",
        "--cwd",
        "/p",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Receipt {
            json,
            html,
            session,
            out,
            cwd,
        }) => {
            assert!(!json);
            assert!(html);
            assert_eq!(session.as_deref(), Some("s-1"));
            assert_eq!(out, Some(std::path::PathBuf::from("RECEIPT.html")));
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected receipt command, got {other:?}"),
    }

    // Bare `tomte receipt` is markdown to stdout.
    let bare = Cli::try_parse_from(["tomte", "receipt"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Receipt {
            json: false,
            html: false,
            session: None,
            out: None,
            cwd: None
        })
    ));

    // One artifact, one format: --json and --html refuse to combine.
    assert!(Cli::try_parse_from(["tomte", "receipt", "--json", "--html"]).is_err());
}

#[test]
fn seal_parses_bare_show_and_verify() {
    // Bare `tomte seal` writes a seal at HEAD; flags default off.
    let bare = Cli::try_parse_from(["tomte", "seal"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Seal {
            action: None,
            json: false,
            cwd: None
        })
    ));

    let json = Cli::try_parse_from(["tomte", "seal", "--json", "--cwd", "/p"]).unwrap();
    match json.command {
        Some(Command::Seal { action, json, cwd }) => {
            assert!(action.is_none());
            assert!(json);
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected seal command, got {other:?}"),
    }

    // `seal show [rev]` — rev optional, defaults to HEAD at run time.
    let show = Cli::try_parse_from(["tomte", "seal", "show", "abc123", "--json"]).unwrap();
    match show.command {
        Some(Command::Seal {
            action: Some(commands::seal::SealAction::Show { rev, json, .. }),
            ..
        }) => {
            assert_eq!(rev, Some("abc123".to_string()));
            assert!(json);
        }
        other => panic!("expected seal show, got {other:?}"),
    }

    // `seal verify` with no rev — the CI-gate spelling.
    let verify = Cli::try_parse_from(["tomte", "seal", "verify"]).unwrap();
    assert!(matches!(
        verify.command,
        Some(Command::Seal {
            action: Some(commands::seal::SealAction::Verify {
                rev: None,
                json: false,
                cwd: None
            }),
            ..
        })
    ));
}

#[test]
fn cost_parses_session_and_cwd() {
    let cli =
        Cli::try_parse_from(["tomte", "cost", "--session", "abc-123", "--cwd", "/p"]).unwrap();
    match cli.command {
        Some(Command::Cost { session, all, cwd }) => {
            assert_eq!(session, Some("abc-123".to_string()));
            assert!(!all, "--all defaults off");
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected cost command, got {other:?}"),
    }
    // All flags are optional — `tomte cost` alone defaults to the newest session.
    let bare = Cli::try_parse_from(["tomte", "cost"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Cost {
            session: None,
            all: false,
            cwd: None
        })
    ));
}

#[test]
fn cost_all_parses_and_conflicts_with_session() {
    let cli = Cli::try_parse_from(["tomte", "cost", "--all"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Command::Cost {
            session: None,
            all: true,
            cwd: None
        })
    ));
    // One report, one scope: --all and --session refuse to combine.
    assert!(Cli::try_parse_from(["tomte", "cost", "--all", "--session", "s-1"]).is_err());
}

#[test]
fn completions_parses_known_shells_and_rejects_unknown() {
    use clap_complete::Shell;
    for (name, want) in [
        ("bash", Shell::Bash),
        ("zsh", Shell::Zsh),
        ("fish", Shell::Fish),
        ("powershell", Shell::PowerShell),
        ("elvish", Shell::Elvish),
    ] {
        let cli = Cli::try_parse_from(["tomte", "completions", name]).unwrap();
        match cli.command {
            Some(Command::Completions { shell }) => assert_eq!(shell, want),
            other => panic!("expected completions command, got {other:?}"),
        }
    }
    // An unknown shell is a parse error with the choices listed, not a panic.
    assert!(Cli::try_parse_from(["tomte", "completions", "tcsh"]).is_err());
    // The shell argument is required.
    assert!(Cli::try_parse_from(["tomte", "completions"]).is_err());
}

#[test]
fn completions_scripts_generate_for_every_shell() {
    use clap::CommandFactory;
    use clap_complete::Shell;
    for shell in [
        Shell::Bash,
        Shell::Zsh,
        Shell::Fish,
        Shell::PowerShell,
        Shell::Elvish,
    ] {
        let mut buf = Vec::new();
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "tomte", &mut buf);
        let script = String::from_utf8(buf).expect("script is utf-8");
        assert!(
            script.contains("tomte"),
            "{shell:?} script must mention the binary"
        );
        // The script must know the subcommands, not just the binary name.
        assert!(
            script.contains("sessions") && script.contains("prove"),
            "{shell:?} script must cover the command surface"
        );
    }
}

#[test]
fn sessions_bare_lists_and_json_flag_parses() {
    let bare = Cli::try_parse_from(["tomte", "sessions"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Sessions {
            action: None,
            json: false,
            cwd: None
        })
    ));

    let json = Cli::try_parse_from(["tomte", "sessions", "--json", "--cwd", "/p"]).unwrap();
    match json.command {
        Some(Command::Sessions { action, json, cwd }) => {
            assert!(action.is_none());
            assert!(json);
            assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
        }
        other => panic!("expected sessions command, got {other:?}"),
    }
}

#[test]
fn sessions_show_parses_optional_id_and_out() {
    let with_id =
        Cli::try_parse_from(["tomte", "sessions", "show", "abc-1", "--out", "t.md"]).unwrap();
    match with_id.command {
        Some(Command::Sessions {
            action: Some(commands::sessions::SessionsAction::Show { id, json, out, .. }),
            ..
        }) => {
            assert_eq!(id.as_deref(), Some("abc-1"));
            assert!(!json, "show defaults to the markdown transcript");
            assert_eq!(out, Some(std::path::PathBuf::from("t.md")));
        }
        other => panic!("expected sessions show, got {other:?}"),
    }

    // Bare `sessions show` — newest session, stdout.
    let bare = Cli::try_parse_from(["tomte", "sessions", "show"]).unwrap();
    assert!(matches!(
        bare.command,
        Some(Command::Sessions {
            action: Some(commands::sessions::SessionsAction::Show {
                id: None,
                json: false,
                out: None,
                cwd: None
            }),
            ..
        })
    ));
}

#[test]
fn sessions_prune_parses_rules_and_defaults_to_dry_run() {
    let cli = Cli::try_parse_from([
        "tomte",
        "sessions",
        "prune",
        "--keep",
        "5",
        "--older-than-days",
        "30",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Sessions {
            action:
                Some(commands::sessions::SessionsAction::Prune {
                    keep,
                    older_than_days,
                    yes,
                    ..
                }),
            ..
        }) => {
            assert_eq!(keep, Some(5));
            assert_eq!(older_than_days, Some(30));
            assert!(!yes, "prune must default to the dry run");
        }
        other => panic!("expected sessions prune, got {other:?}"),
    }

    let armed =
        Cli::try_parse_from(["tomte", "sessions", "prune", "--keep", "3", "--yes"]).unwrap();
    match armed.command {
        Some(Command::Sessions {
            action: Some(commands::sessions::SessionsAction::Prune { yes, .. }),
            ..
        }) => assert!(yes),
        other => panic!("expected sessions prune, got {other:?}"),
    }
}

#[test]
fn mcp_add_parses_name_env_and_trailing_command() {
    let cli = Cli::try_parse_from([
        "tomte", "mcp", "add", "fs", "--env", "K=V", "--", "npx", "-y", "server", "/tmp",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Mcp {
            action: Some(commands::mcp::McpAction::Add { name, env, command }),
        }) => {
            assert_eq!(name, "fs");
            assert_eq!(env, vec!["K=V".to_string()]);
            assert_eq!(
                command,
                ["npx", "-y", "server", "/tmp"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            );
        }
        other => panic!("expected mcp add, got {other:?}"),
    }
}

#[test]
fn mcp_remove_accepts_rm_alias_and_bare_lists() {
    let aliased = Cli::try_parse_from(["tomte", "mcp", "rm", "filesystem"]).unwrap();
    assert!(matches!(
        aliased.command,
        Some(Command::Mcp {
            action: Some(commands::mcp::McpAction::Remove { .. })
        })
    ));
    // `tomte mcp` with no action defaults to the list view.
    let bare = Cli::try_parse_from(["tomte", "mcp"]).unwrap();
    assert!(matches!(bare.command, Some(Command::Mcp { action: None })));
}

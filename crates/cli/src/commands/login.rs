use anyhow::Result;
use opencli_core::auth::{self, anthropic as anth_oauth, AuthMode};
use opencli_core::provider::Provider;
use std::io::{BufRead, IsTerminal, Write};

pub async fn run(api_key: bool, open_browser: bool, provider: Option<String>) -> Result<()> {
    if let Some(p) = provider.as_deref() {
        let result = match (api_key, p) {
            (true, "openai") => login_openai_api_key().await,
            (true, "anthropic") => login_anthropic_api_key().await,
            (false, "openai") => auth::login_with_browser(open_browser).await.map(|_| ()),
            (false, "anthropic") => login_anthropic_oauth(open_browser).await,
            _ => anyhow::bail!("unknown provider `{p}`; expected `openai` or `anthropic`"),
        };
        result?;
        print_available_models();
        return Ok(());
    }
    if api_key {
        // No provider specified — ask which one.
        let chosen = prompt_api_key_provider()?;
        match chosen.as_str() {
            "anthropic" => login_anthropic_api_key().await?,
            _ => login_openai_api_key().await?,
        }
        print_available_models();
        return Ok(());
    }
    show_login_picker(open_browser).await?;
    print_available_models();
    Ok(())
}

async fn show_login_picker(open_browser: bool) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        auth::login_with_browser(open_browser).await?;
        return Ok(());
    }
    println!();
    println!("  Sign in to opencli");
    println!("  ─────────────────────");
    println!("    [1] OpenAI — ChatGPT account (OAuth in browser)");
    println!("    [2] OpenAI — API key");
    println!("    [3] Anthropic — Claude Pro/Max (OAuth, may violate ToS)");
    println!("    [4] Anthropic — Console API key");
    println!("    [q] Cancel");
    println!();
    print!("  Choose an option [1-4]: ");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf)?;
    match buf.trim() {
        "1" => auth::login_with_browser(open_browser).await.map(|_| ()),
        "2" => login_openai_api_key().await,
        "3" => login_anthropic_oauth(open_browser).await,
        "4" => login_anthropic_api_key().await,
        "q" | "Q" | "" => {
            println!("Cancelled.");
            Ok(())
        }
        other => anyhow::bail!("unrecognised choice `{other}`"),
    }
}

async fn login_openai_api_key() -> Result<()> {
    let key = prompt_secret("Paste your OpenAI API key (sk-…) and press Enter:")?;
    if key.is_empty() {
        anyhow::bail!("API key is empty");
    }
    let mut record = auth::load_auth().unwrap_or_default();
    auth::activate_openai_api_key(&mut record, key);
    auth::save_auth(&record)?;
    println!("✅  OpenAI API key saved.");
    Ok(())
}

async fn login_anthropic_api_key() -> Result<()> {
    let key = prompt_secret("Paste your Anthropic API key (sk-ant-…) and press Enter:")?;
    if key.is_empty() {
        anyhow::bail!("API key is empty");
    }
    let mut record = auth::load_auth().unwrap_or_default();
    auth::activate_anthropic_api_key(&mut record, key);
    auth::save_auth(&record)?;
    println!("✅  Anthropic API key saved.");
    Ok(())
}

async fn login_anthropic_oauth(open_browser: bool) -> Result<()> {
    println!();
    println!("{}", anth_oauth::TOS_WARNING);
    print!("  Type `i-accept` to continue, anything else to cancel: ");
    std::io::stdout().flush().ok();
    let mut ack = String::new();
    std::io::stdin().lock().read_line(&mut ack)?;
    if ack.trim() != "i-accept" {
        println!("Cancelled.");
        return Ok(());
    }
    let login = anth_oauth::begin_manual_login(open_browser);
    println!();
    println!("  Open this URL in your browser to sign in with Claude:");
    println!("     {}", login.auth_url);
    println!();
    println!("  After you approve, claude.ai shows an authorization code.");
    print!("  Paste the code here and press Enter: ");
    std::io::stdout().flush().ok();
    let mut code = String::new();
    std::io::stdin().lock().read_line(&mut code)?;
    anth_oauth::complete_manual_login(&login, code.trim()).await?;
    println!("  Signed in with Claude.");
    Ok(())
}

fn prompt_api_key_provider() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        // Non-interactive: default to openai for backward compat.
        return Ok("openai".to_string());
    }
    println!();
    println!("  Which provider is this API key for?");
    println!("    [1] OpenAI  (sk-…)");
    println!("    [2] Anthropic  (sk-ant-…)");
    println!();
    print!("  Choose [1/2]: ");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf)?;
    match buf.trim() {
        "2" => Ok("anthropic".to_string()),
        _ => Ok("openai".to_string()),
    }
}

fn prompt_secret(prompt: &str) -> Result<String> {
    eprintln!("{prompt}");
    // Non-TTY (piped) stdin: read normally — nothing is echoed to a terminal.
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().lock().read_line(&mut buf)?;
        return Ok(buf.trim().to_string());
    }
    // TTY: read with echo OFF so the secret isn't displayed as typed or left in
    // scrollback. Raw mode disables line editing, so handle Enter/Backspace and
    // Ctrl+C ourselves.
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    crossterm::terminal::enable_raw_mode()?;
    let mut buf = String::new();
    let outcome: Result<()> = loop {
        match event::read() {
            Ok(Event::Key(k)) => match (k.code, k.modifiers) {
                (KeyCode::Enter, _) => break Ok(()),
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("aborted"))
                }
                (KeyCode::Backspace, _) => {
                    buf.pop();
                }
                (KeyCode::Char(c), _) => buf.push(c),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    eprintln!();
    outcome.map(|_| buf.trim().to_string())
}

/// Print the catalogue of models available for every provider the user is
/// currently signed in to. Hidden entirely when no credentials exist so the
/// user sees a clean prompt after `logout`.
fn print_available_models() {
    let catalogs = auth::signed_in_model_catalogs();
    if catalogs.is_empty() {
        return;
    }
    println!();
    println!("  Available models:");
    for catalog in catalogs {
        println!(
            "    {} ({}):",
            catalog.provider.display_name(),
            catalog.provider
        );
        for m in catalog.models {
            let win = opencli_core::agent::context_window_label(m);
            println!("      · {m:<20} ({win} context)");
        }
    }
    println!();
    println!(
        "  Set the active model with `opencli config --set-model <id>` or `/model <id>` in the TUI."
    );
}

pub async fn status() -> Result<()> {
    let record = auth::load_auth()?;
    let active_mode = auth::effective_mode_with_env(&record);
    match active_mode {
        AuthMode::None => println!("Not signed in."),
        AuthMode::OpenaiApiKey => println!("OpenAI: signed in with API key."),
        AuthMode::OpenaiOauth => {
            let acc = record
                .tokens
                .as_ref()
                .and_then(|t| t.account_id.clone())
                .unwrap_or_else(|| "(no account id)".to_string());
            println!("OpenAI: signed in with ChatGPT, account_id={acc}");
        }
        AuthMode::AnthropicApiKey => println!("Anthropic: signed in with API key."),
        AuthMode::AnthropicOauth => {
            println!("Anthropic: signed in with Claude Pro/Max OAuth.");
        }
    }
    if auth::has_anthropic_oauth(&record) && !matches!(active_mode, AuthMode::AnthropicOauth) {
        println!("  (Anthropic OAuth token is also stored)");
    }
    if auth::has_anthropic_api_key(&record) && !matches!(active_mode, AuthMode::AnthropicApiKey) {
        println!("  (Anthropic API key is also stored)");
    }
    if auth::has_openai_oauth(&record) && !matches!(active_mode, AuthMode::OpenaiOauth) {
        println!("  (OpenAI OAuth token is also stored)");
    }
    if auth::has_openai_api_key(&record) && !matches!(active_mode, AuthMode::OpenaiApiKey) {
        println!("  (OpenAI API key is also stored)");
    }
    let coverage = auth::credential_coverage();
    println!();
    println!("  Credential coverage:");
    println!("    OpenAI OAuth:       {}", coverage.openai_oauth.label());
    println!(
        "    OpenAI API key:     {}",
        coverage.openai_api_key.label()
    );
    println!(
        "    Anthropic OAuth:    {}",
        coverage.anthropic_oauth.label()
    );
    println!(
        "    Anthropic API key:  {}",
        coverage.anthropic_api_key.label()
    );
    print_available_models();
    Ok(())
}

pub async fn logout() -> Result<()> {
    let path = opencli_core::config::config_dir().join("auth.json");
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("✅  Credentials cleared.");
    } else {
        println!("Nothing to clear.");
    }
    // Intentionally do not print the model catalogue after logout — the
    // signed_in_providers() check would normally hide it, but env-var
    // credentials still exist after logout. Force-hide here.
    Ok(())
}

// Force-suppress dead-code warning for Provider import when it ends up unused
// in CI builds with a different feature flag.
#[allow(dead_code)]
fn _provider_used() -> Provider {
    Provider::OpenAi
}

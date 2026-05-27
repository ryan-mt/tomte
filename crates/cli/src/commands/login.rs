use anyhow::Result;
use opencli_core::auth::{self, AuthMode, AuthRecord};

pub async fn run(api_key: bool, open_browser: bool) -> Result<()> {
    if api_key {
        eprintln!("Paste your OpenAI API key (sk-…) and press Enter:");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        let key = buf.trim().to_string();
        if key.is_empty() {
            anyhow::bail!("API key is empty");
        }
        let record = AuthRecord {
            mode: AuthMode::ApiKey,
            api_key: Some(key),
            tokens: None,
            last_refresh: None,
        };
        auth::save_auth(&record)?;
        println!("✅  API key saved.");
        return Ok(());
    }
    auth::login_with_browser(open_browser).await?;
    Ok(())
}

pub async fn status() -> Result<()> {
    let record = auth::load_auth()?;
    match record.mode {
        AuthMode::None => println!("Not signed in."),
        AuthMode::ApiKey => println!("Signed in with API key."),
        AuthMode::ChatGPT => {
            let acc = record
                .tokens
                .as_ref()
                .and_then(|t| t.account_id.clone())
                .unwrap_or_else(|| "(no account id)".to_string());
            println!("Signed in with ChatGPT, account_id={acc}");
        }
    }
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
    Ok(())
}

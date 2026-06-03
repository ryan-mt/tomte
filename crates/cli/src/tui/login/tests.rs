use super::*;
use tokio::sync::Mutex as AsyncMutex;

static ENV_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[tokio::test]
async fn api_key_entry_does_not_prefill_env_secret() {
    let _lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::set("OPENAI_API_KEY", "sk-secret-should-not-render");
    let mut login = LoginScreen::new();
    login.selected = Option_::OpenAiApiKey;

    let finished = login
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(!finished);
    assert!(matches!(
        login.stage().await,
        Stage::ApiKeyEntry {
            provider: Provider::OpenAi
        }
    ));
    assert!(login.api_input.is_empty());
}

#[tokio::test]
async fn anthropic_oauth_routes_through_tos_gate() {
    let mut login = LoginScreen::new();
    login.selected = Option_::AnthropicOauth;

    let finished = login
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(!finished);
    assert!(matches!(login.stage().await, Stage::AnthropicTos));
}

#[tokio::test]
async fn esc_in_waiting_for_browser_bumps_generation() {
    let mut login = LoginScreen::new();
    *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
    let before = login.flow_generation.load(Ordering::SeqCst);

    let finished = login
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(!finished);
    assert!(matches!(login.stage().await, Stage::PickMode));
    assert_ne!(login.flow_generation.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn stale_chatgpt_result_is_dropped() {
    // A completion task from an abandoned flow must not clobber the screen.
    let login = LoginScreen::new();
    *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
    let my_gen = login
        .flow_generation
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);
    // User abandons the flow (Esc/Ctrl+C bumps the generation).
    login.flow_generation.fetch_add(1, Ordering::SeqCst);
    *login.stage.lock().await = Stage::PickMode;

    // The late OAuth success arrives — it must be ignored.
    LoginScreen::finish_chatgpt(
        &login.stage,
        &login.error,
        &login.flow_generation,
        my_gen,
        Ok(()),
    )
    .await;

    assert!(matches!(login.stage().await, Stage::PickMode));
    assert!(login.error_text().await.is_none());
}

#[tokio::test]
async fn current_chatgpt_result_is_applied() {
    let login = LoginScreen::new();
    *login.stage.lock().await = Stage::WaitingForBrowser { url: "x".into() };
    let my_gen = login
        .flow_generation
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);

    LoginScreen::finish_chatgpt(
        &login.stage,
        &login.error,
        &login.flow_generation,
        my_gen,
        Ok(()),
    )
    .await;

    assert!(matches!(
        login.stage().await,
        Stage::Success(AuthMode::OpenaiOauth)
    ));
}

//! Onboarding / login screen — first screen when the user is not authenticated.
//!
//! Offers the same four sign-in paths as `tomte login`:
//!   ▸ OpenAI — ChatGPT account (OAuth via a local browser callback)
//!   ▸ OpenAI — API key
//!   ▸ Anthropic — Claude Pro/Max (OAuth, manual code paste, may violate ToS)
//!   ▸ Anthropic — Console API key
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::Mutex;
use tomte_core::auth::{self, anthropic as anth, AuthMode};
use tomte_core::provider::Provider;

use super::input::TextInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Option_ {
    OpenAiChatGpt,
    OpenAiApiKey,
    AnthropicOauth,
    AnthropicApiKey,
}

impl Option_ {
    const ALL: [Option_; 4] = [
        Option_::OpenAiChatGpt,
        Option_::OpenAiApiKey,
        Option_::AnthropicOauth,
        Option_::AnthropicApiKey,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone)]
pub enum Stage {
    PickMode,
    /// OpenAI OAuth: waiting on the local browser callback.
    WaitingForBrowser {
        url: String,
    },
    /// API-key entry, shared by both providers.
    ApiKeyEntry {
        provider: Provider,
    },
    /// Anthropic OAuth: ToS gate shown before the flow starts.
    AnthropicTos,
    /// Anthropic OAuth: URL shown, waiting for the user to paste the code.
    AnthropicPaste {
        url: String,
    },
    Success(AuthMode),
    Cancelled,
}

pub struct LoginScreen {
    pub stage: Arc<Mutex<Stage>>,
    pub selected: Option_,
    pub api_input: TextInput,
    pub paste_input: TextInput,
    pub error: Arc<Mutex<Option<String>>>,
    /// Held between [`Stage::AnthropicPaste`] start and code submission. The
    /// Claude OAuth client has no loopback redirect, so there is no callback —
    /// the user pastes the code and we finish the exchange here.
    anth_pending: Option<anth::ManualLogin>,
    /// Bumped whenever a ChatGPT OAuth flow is abandoned (Esc / Ctrl+C) or a new
    /// one is started. The spawned completion task applies its result only if
    /// this still equals the value it captured at spawn, so a callback that
    /// fires after the user moved on can't clobber the screen (e.g. flip it to
    /// an unexpected `Success` or overwrite a fresh flow with a stale error).
    flow_generation: Arc<AtomicU64>,
}

impl LoginScreen {
    pub fn new() -> Self {
        Self {
            stage: Arc::new(Mutex::new(Stage::PickMode)),
            selected: Option_::OpenAiChatGpt,
            api_input: TextInput::default(),
            paste_input: TextInput::default(),
            error: Arc::new(Mutex::new(None)),
            anth_pending: None,
            flow_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn stage(&self) -> Stage {
        self.stage.lock().await.clone()
    }

    pub async fn error_text(&self) -> Option<String> {
        self.error.lock().await.clone()
    }

    /// Handle a key event. Returns Ok(true) when the screen is finished.
    pub async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            // Invalidate any in-flight OAuth completion task before leaving.
            self.flow_generation.fetch_add(1, Ordering::SeqCst);
            *self.stage.lock().await = Stage::Cancelled;
            return Ok(true);
        }
        let stage = self.stage().await;
        match stage {
            Stage::PickMode => self.handle_pick(key).await,
            Stage::WaitingForBrowser { .. } => {
                if key.code == KeyCode::Esc {
                    // Abandon the flow: invalidate the pending completion task so
                    // a late browser callback can't reopen or "succeed" the screen.
                    self.flow_generation.fetch_add(1, Ordering::SeqCst);
                    *self.stage.lock().await = Stage::PickMode;
                }
                Ok(false)
            }
            Stage::ApiKeyEntry { provider } => self.handle_api_key(key, provider).await,
            Stage::AnthropicTos => self.handle_tos(key).await,
            Stage::AnthropicPaste { .. } => self.handle_paste(key).await,
            Stage::Success(_) | Stage::Cancelled => Ok(true),
        }
    }

    async fn handle_pick(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.prev(),
            KeyCode::Down | KeyCode::Char('j') => self.selected = self.selected.next(),
            KeyCode::Char('1') => self.selected = Option_::OpenAiChatGpt,
            KeyCode::Char('2') => self.selected = Option_::OpenAiApiKey,
            KeyCode::Char('3') => self.selected = Option_::AnthropicOauth,
            KeyCode::Char('4') => self.selected = Option_::AnthropicApiKey,
            KeyCode::Enter => match self.selected {
                Option_::OpenAiChatGpt => self.start_chatgpt().await,
                Option_::OpenAiApiKey => {
                    self.api_input.clear();
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::ApiKeyEntry {
                        provider: Provider::OpenAi,
                    };
                }
                Option_::AnthropicOauth => {
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::AnthropicTos;
                }
                Option_::AnthropicApiKey => {
                    self.api_input.clear();
                    *self.error.lock().await = None;
                    *self.stage.lock().await = Stage::ApiKeyEntry {
                        provider: Provider::Anthropic,
                    };
                }
            },
            _ => {}
        }
        Ok(false)
    }

    async fn handle_api_key(&mut self, key: KeyEvent, provider: Provider) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                let key_str = self.api_input.buffer.trim().to_string();
                if key_str.is_empty() {
                    *self.error.lock().await = Some("API key cannot be empty".into());
                    return Ok(false);
                }
                let mut record = auth::load_auth().unwrap_or_default();
                let mode = match provider {
                    Provider::OpenAi => {
                        auth::activate_openai_api_key(&mut record, key_str);
                        AuthMode::OpenaiApiKey
                    }
                    Provider::Anthropic => {
                        auth::activate_anthropic_api_key(&mut record, key_str);
                        AuthMode::AnthropicApiKey
                    }
                };
                match auth::save_auth(&record) {
                    Ok(_) => {
                        *self.stage.lock().await = Stage::Success(mode);
                        return Ok(true);
                    }
                    Err(e) => {
                        *self.error.lock().await = Some(format!("Failed to save: {e}"));
                    }
                }
            }
            KeyCode::Backspace => self.api_input.backspace(),
            KeyCode::Char('u') if ctrl => self.api_input.clear(),
            KeyCode::Char('w') if ctrl => self.api_input.delete_word_left(),
            KeyCode::Char(c) if !ctrl => self.api_input.insert_char(c),
            KeyCode::Left => self.api_input.move_left(),
            KeyCode::Right => self.api_input.move_right(),
            KeyCode::Home => self.api_input.move_home(),
            KeyCode::End => self.api_input.move_end(),
            _ => {}
        }
        Ok(false)
    }

    async fn handle_tos(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                // Build the authorize URL and open the browser. Claude's OAuth
                // client registers no loopback redirect, so the user copies the
                // code shown by claude.ai back into the paste field.
                let login = anth::begin_manual_login(true);
                let url = login.auth_url.clone();
                self.anth_pending = Some(login);
                self.paste_input.clear();
                *self.error.lock().await = None;
                *self.stage.lock().await = Stage::AnthropicPaste { url };
            }
            _ => {}
        }
        Ok(false)
    }

    async fn handle_paste(&mut self, key: KeyEvent) -> Result<bool> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.anth_pending = None;
                *self.stage.lock().await = Stage::PickMode;
            }
            KeyCode::Enter => {
                let code = self.paste_input.buffer.trim().to_string();
                if code.is_empty() {
                    *self.error.lock().await = Some("Paste the code from the browser".into());
                    return Ok(false);
                }
                // Borrow ends when `result` is produced, freeing self for the
                // mutation/lock calls below.
                let result = match self.anth_pending.as_ref() {
                    Some(login) => Some(anth::complete_manual_login(login, &code).await),
                    None => None,
                };
                match result {
                    None => {
                        *self.error.lock().await = Some("Login expired; start again".into());
                        *self.stage.lock().await = Stage::PickMode;
                    }
                    Some(Ok(_)) => {
                        self.anth_pending = None;
                        *self.stage.lock().await = Stage::Success(AuthMode::AnthropicOauth);
                        return Ok(true);
                    }
                    Some(Err(e)) => {
                        *self.error.lock().await = Some(e.to_string());
                    }
                }
            }
            KeyCode::Backspace => self.paste_input.backspace(),
            KeyCode::Char('u') if ctrl => self.paste_input.clear(),
            KeyCode::Char('w') if ctrl => self.paste_input.delete_word_left(),
            KeyCode::Char(c) if !ctrl => self.paste_input.insert_char(c),
            KeyCode::Left => self.paste_input.move_left(),
            KeyCode::Right => self.paste_input.move_right(),
            KeyCode::Home => self.paste_input.move_home(),
            KeyCode::End => self.paste_input.move_end(),
            _ => {}
        }
        Ok(false)
    }

    async fn start_chatgpt(&mut self) {
        // Claim a fresh generation so this flow's completion task can tell
        // whether it's still current when it finishes (also supersedes any
        // previous flow's task on re-entry).
        let my_gen = self
            .flow_generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let stage = self.stage.clone();
        let err = self.error.clone();
        // Start the OAuth flow. This returns immediately with the auth URL.
        match auth::start_browser_login(true).await {
            Ok(pending) => {
                *stage.lock().await = Stage::WaitingForBrowser {
                    url: pending.auth_url.clone(),
                };
                let stage2 = stage.clone();
                let err2 = err.clone();
                let gen2 = self.flow_generation.clone();
                tokio::spawn(async move {
                    let outcome = match pending.completion.await {
                        Ok(Ok(_)) => Ok(()),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(e) => Err(format!("login task crashed: {e}")),
                    };
                    Self::finish_chatgpt(&stage2, &err2, &gen2, my_gen, outcome).await;
                });
            }
            Err(e) => {
                *err.lock().await = Some(e.to_string());
                *stage.lock().await = Stage::PickMode;
            }
        }
    }

    /// Apply a ChatGPT OAuth completion, but only if its flow is still current:
    /// if the user pressed Esc/Ctrl+C or started another flow, `flow_generation`
    /// has moved past `my_gen` and the (now stale) result is dropped instead of
    /// clobbering the screen. The generation is read under the `stage` lock so it
    /// stays consistent with the state being written.
    async fn finish_chatgpt(
        stage: &Arc<Mutex<Stage>>,
        err: &Arc<Mutex<Option<String>>>,
        generation: &Arc<AtomicU64>,
        my_gen: u64,
        outcome: Result<(), String>,
    ) {
        let mut s = stage.lock().await;
        if generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        match outcome {
            Ok(()) => *s = Stage::Success(AuthMode::OpenaiOauth),
            Err(msg) => {
                *err.lock().await = Some(msg);
                *s = Stage::PickMode;
            }
        }
    }
}

mod render;
pub use render::render;

#[cfg(test)]
mod tests;

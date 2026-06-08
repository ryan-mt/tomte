//! Live OAuth smoke test for the OpenAI (ChatGPT/Codex subscription) path.
//!
//! Marked `#[ignore]`: it reads the REAL stored credential from the OS config
//! dir and makes ONE minimal authenticated request to the live ChatGPT/Codex
//! backend, so it must never run in the default `cargo test`/CI sweep. Run it
//! explicitly:
//!
//!   cargo test -p tomte-core --test oauth_openai_live -- --ignored --nocapture
//!
//! It does NOT mutate `auth.json` under normal conditions: `ensure_fresh` only
//! refreshes (and rotates the single-use refresh token) when the access token
//! is within ~2 min of expiry; a still-valid token is returned untouched. The
//! request sets `store: false` (the client does this for subscription creds),
//! so nothing is persisted server-side either.
//!
//! The ChatGPT/Codex OAuth backend requires `stream: true` (a non-streaming
//! `create()` is rejected with `400 {"detail":"Stream must be set to true"}`),
//! so this drives the streaming path — the same one a real OAuth turn uses.

use tomte_core::auth::{self, Credential};
use tomte_core::openai::{
    InputItem, MessageContent, OpenAiClient, ResponseStreamEvent, ResponsesRequest,
};
use tomte_core::provider::Provider;

#[tokio::test]
#[ignore = "hits the live ChatGPT/Codex backend with the stored OAuth credential; run with --ignored"]
async fn openai_oauth_credential_authenticates_against_live_backend() {
    // 1. Load the real stored auth record (reads the OS config dir, honoring
    //    TOMTE_CONFIG_DIR on every platform).
    let record = auth::load_auth().expect("load auth.json");
    let tokens = record
        .tokens
        .as_ref()
        .expect("no OpenAI OAuth tokens stored — run `tomte login` first");
    assert!(
        !tokens.access_token.is_empty(),
        "stored access_token is empty"
    );
    assert!(
        !tokens.refresh_token.is_empty(),
        "stored refresh_token is empty"
    );

    println!("[1] auth.json loaded:");
    println!("      mode               = {:?}", record.mode);
    println!("      access_token_len   = {}", tokens.access_token.len());
    println!("      refresh_token_len  = {}", tokens.refresh_token.len());
    println!("      id_token_present   = {}", tokens.id_token.is_some());
    println!("      account_id_present = {}", tokens.account_id.is_some());
    println!("      expires_at         = {:?}", tokens.expires_at);

    // 2. ensure_fresh returns a usable access token, refreshing (and rotating
    //    the single-use refresh token) only if within ~2 min of expiry. Compare
    //    with the stored token to report whether a rotation happened.
    let stored_access = tokens.access_token.clone();
    let access = auth::oauth::ensure_fresh(&record)
        .await
        .expect("ensure_fresh failed");
    assert!(!access.is_empty(), "ensure_fresh returned an empty token");
    let refreshed = access != stored_access;
    println!(
        "[2] ensure_fresh OK (token_len={}, refreshed={})",
        access.len(),
        refreshed
    );

    // 3. Build the OAuth credential exactly as the OpenAI resolver does.
    let credential = Credential::OAuth {
        provider: Provider::OpenAi,
        access_token: access,
        account_id: tokens.account_id.clone(),
    };
    assert!(
        credential.is_chatgpt_subscription(),
        "OpenAI OAuth credential must route through the ChatGPT/Codex backend"
    );

    // 4. One minimal authenticated streaming request to the live backend.
    //    `gpt-5.5` is one of the two model ids the ChatGPT/Codex OAuth backend
    //    accepts. `.stream()` sets `stream: true`, which this backend requires.
    let client = OpenAiClient::new(credential).expect("build client");
    let req = ResponsesRequest::new(
        "gpt-5.5",
        vec![InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(
                "Reply with exactly this token and nothing else: OAUTH_OK",
            )],
        }],
    )
    .with_instructions("You are a connectivity probe. Output only what is requested.");

    println!("[3] opening live stream to the ChatGPT/Codex backend (model=gpt-5.5)...");
    let mut handle = client
        .stream(req)
        .await
        .expect("live OpenAI stream request failed — the OAuth token was not accepted");

    // Pump SSE events: accumulate output-text deltas until a terminal event.
    let mut text = String::new();
    let mut done_text = String::new();
    let mut saw_completed = false;
    while let Some(ev) = handle.rx.recv().await {
        match ev {
            Ok(ResponseStreamEvent::OutputTextDelta { delta, .. }) => text.push_str(&delta),
            Ok(ResponseStreamEvent::OutputTextDone { text: t, .. }) => done_text = t,
            Ok(ResponseStreamEvent::Completed { .. }) => {
                saw_completed = true;
                break;
            }
            Ok(ResponseStreamEvent::Failed { response }) => {
                panic!("stream reported a failed response: {response}")
            }
            Ok(ResponseStreamEvent::Error { message }) => panic!("stream error event: {message}"),
            Ok(_) => {} // reasoning deltas, item lifecycle, rate-limit events, etc.
            Err(e) => panic!("stream transport error: {e}"),
        }
    }
    assert!(
        saw_completed,
        "stream ended without a terminal `response.completed` event"
    );

    let final_text = if text.trim().is_empty() {
        done_text
    } else {
        text
    };
    println!("[4] live response output_text = {:?}", final_text.trim());
    assert!(
        !final_text.trim().is_empty(),
        "expected non-empty model output from the live backend"
    );

    println!(
        "\n✅ OAUTH OPENAI LIVE TEST PASSED — the stored OAuth token authenticated \
         and the live backend streamed output."
    );
}

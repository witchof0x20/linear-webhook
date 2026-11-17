//! Regression test: serenity's `tracing` spans include the webhook token and,
//! without a tracing subscriber, are forwarded to the `log` crate (and from
//! there to Rocket's logger / the journal). Nothing logged may contain it.
//!
//! This lives in its own test binary because it installs a global logger.

mod common;

use std::sync::Mutex;

use log::{Log, Metadata, Record};

use linear_webhook::config::DiscordWebhook;
use linear_webhook::discord;
use linear_webhook::payload::Payload;
use linear_webhook::render::render;

use common::fake_discord;

const TOKEN: &str = "SECRETTOKENaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl Log for Capture {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }
    fn log(&self, record: &Record) {
        LINES
            .lock()
            .unwrap()
            .push(format!("{} {}", record.target(), record.args()));
    }
    fn flush(&self) {}
}

#[rocket::async_test]
async fn webhook_token_is_never_logged() {
    log::set_boxed_logger(Box::new(Capture)).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let webhook = DiscordWebhook::parse(&format!(
        "https://discord.com/api/webhooks/123456789012345678/{TOKEN}"
    ))
    .unwrap();
    let body = include_bytes!("../examples/comment-create.json");

    let (base, captured) = fake_discord("204 No Content", "").await;
    let message = render(&Payload::parse(body).unwrap(), body, None);
    discord::send(&discord::client(Some(&base)), &webhook, message)
        .await
        .unwrap();
    assert_eq!(captured.lock().unwrap().len(), 1);

    // A rejected send: serenity's error carries the request URL (and so the
    // token), and the server logs the error's Display.
    let (base, _) = fake_discord(
        "404 Not Found",
        r#"{"message": "Unknown Webhook", "code": 10015}"#,
    )
    .await;
    let message = render(&Payload::parse(body).unwrap(), body, None);
    let err = discord::send(&discord::client(Some(&base)), &webhook, message)
        .await
        .unwrap_err();
    assert!(!err.is_retryable(), "{err}");
    let shown = err.to_string();
    assert!(!shown.contains(TOKEN), "token leaked into error: {shown}");

    let lines = LINES.lock().unwrap();
    let leaked: Vec<_> = lines.iter().filter(|l| l.contains(TOKEN)).collect();
    assert!(leaked.is_empty(), "token leaked into logs: {leaked:#?}");
}

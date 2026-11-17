//! Regression test: serenity's `tracing` spans include the webhook token and,
//! without a tracing subscriber, are forwarded to the `log` crate (and from
//! there to Rocket's logger / the journal). Nothing logged may contain it.
//!
//! This lives in its own test binary because it installs a global logger.

use std::sync::Mutex;

use log::{Log, Metadata, Record};
use rocket::tokio::io::{AsyncReadExt, AsyncWriteExt};
use rocket::tokio::net::TcpListener;

use linear_webhook::config::DiscordWebhook;
use linear_webhook::discord;
use linear_webhook::payload::Payload;
use linear_webhook::render::render;

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

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    rocket::tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 65536];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    });

    let webhook = DiscordWebhook::parse(&format!(
        "https://discord.com/api/webhooks/123456789012345678/{TOKEN}"
    ))
    .unwrap();
    let body = include_bytes!("../examples/comment-create.json");
    let message = render(&Payload::parse(body).unwrap(), body, None);
    let http = discord::client(Some(&base));
    // The outcome doesn't matter (the fake server may reply before reading the
    // whole body); only what was logged along the way.
    let _ = discord::send(&http, &webhook, message).await;

    // A rejected send: serenity's error carries the request URL (and so the
    // token), and the server logs the error's Display.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    rocket::tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 65536];
        let _ = sock.read(&mut buf).await;
        let body = r#"{"message": "Unknown Webhook", "code": 10015}"#;
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
    });
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

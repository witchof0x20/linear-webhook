//! End-to-end tests: Rocket local client in front, fake Discord behind.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{KeyInit, Mac};
use rocket::figment::Figment;
use rocket::http::{Header, Status};
use rocket::local::asynchronous::Client;
use rocket::tokio::io::{AsyncReadExt, AsyncWriteExt};
use rocket::tokio::net::TcpListener;

use linear_webhook::config::Config;
use linear_webhook::discord;
use linear_webhook::server::{self, App};

const SECRET: &str = "lin_wh_integration";
const WEBHOOK: &str = "https://discord.com/api/webhooks/123456789012345678/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A captured request to the fake Discord: (request line, body).
type Captured = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// Minimal HTTP/1.1 server answering every request with `status`.
async fn fake_discord(status: &'static str) -> (String, Captured) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let captured: Captured = Arc::default();
    let sink = captured.clone();
    rocket::tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            // Read headers.
            let header_end = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while buf.len() < header_end + len {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let request_line = head.lines().next().unwrap().to_owned();
            sink.lock()
                .unwrap()
                .push((request_line, buf[header_end..].to_vec()));
            let resp =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });
    (base, captured)
}

async fn client(discord_status: &'static str, ignore_secret: bool) -> (Client, Captured) {
    let (base, captured) = fake_discord(discord_status).await;
    let config = Config::parse(
        &format!("[team]\ndiscord_webhook_url = \"{WEBHOOK}\"\nlinear_secret = \"{SECRET}\"\n"),
        ignore_secret,
    )
    .unwrap();
    let app = App {
        config,
        http: discord::client(Some(&base)),
    };
    let figment = Figment::from(rocket::Config::debug_default()).merge(("log_level", "off"));
    (
        Client::tracked(server::rocket(figment, app)).await.unwrap(),
        captured,
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn body_with_timestamp(ts: u64) -> Vec<u8> {
    let mut v: serde_json::Value =
        serde_json::from_slice(include_bytes!("../examples/issue-update.json")).unwrap();
    v["webhookTimestamp"] = ts.into();
    serde_json::to_vec(&v).unwrap()
}

fn sign(body: &[u8]) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

async fn post(client: &Client, path: &str, body: Vec<u8>, sig: Option<String>) -> Status {
    let mut req = client
        .post(path)
        .header(Header::new(
            "Linear-Delivery",
            "d3b7c1e0-0000-4000-8000-000000000001",
        ))
        .body(body);
    if let Some(sig) = sig {
        req = req.header(Header::new("Linear-Signature", sig));
    }
    req.dispatch().await.status()
}

#[rocket::async_test]
async fn forwards_valid_delivery() {
    let (client, captured) = client("204 No Content", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);
    assert_eq!(post(&client, "/team", body, Some(sig)).await, Status::Ok);

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let (line, req_body) = &captured[0];
    assert!(
        line.starts_with("POST /api/v10/webhooks/123456789012345678/aaaa"),
        "{line}"
    );
    let req_body = String::from_utf8_lossy(req_body);
    assert!(req_body.contains("Issue updated: ENG-42"), "{req_body}");
    assert!(req_body.contains("linear-Issue-update-d3b7c1e0-0000-4000-8000-000000000001.json.zst"));
    assert!(
        req_body.contains(r#""allowed_mentions":{"parse":[]"#),
        "{req_body}"
    );
}

#[rocket::async_test]
async fn rejects_unauthenticated() {
    let (client, captured) = client("204 No Content", false).await;
    let body = body_with_timestamp(now_ms());

    assert_eq!(
        post(&client, "/team", body.clone(), None).await,
        Status::Unauthorized
    );
    assert_eq!(
        post(&client, "/team", body.clone(), Some("00".repeat(32))).await,
        Status::Unauthorized
    );
    assert_eq!(
        post(&client, "/team", body.clone(), Some("nothex".into())).await,
        Status::Unauthorized
    );

    // Correctly signed, but replayed after the 60 s window.
    let stale = body_with_timestamp(now_ms() - 120_000);
    let sig = sign(&stale);
    assert_eq!(
        post(&client, "/team", stale, Some(sig)).await,
        Status::Unauthorized
    );

    assert!(captured.lock().unwrap().is_empty());
}

#[rocket::async_test]
async fn rejects_bad_requests() {
    let (client, captured) = client("204 No Content", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);

    assert_eq!(
        post(&client, "/nope", body.clone(), Some(sig.clone())).await,
        Status::NotFound
    );

    let junk = b"{\"not\":\"linear\"}".to_vec();
    let junk_sig = sign(&junk);
    assert_eq!(
        post(&client, "/team", junk, Some(junk_sig)).await,
        Status::BadRequest
    );

    let huge = vec![b' '; 2 * 1024 * 1024];
    assert_eq!(
        post(&client, "/team", huge, None).await,
        Status::PayloadTooLarge
    );

    let status = client
        .post("/team")
        .header(Header::new("Linear-Delivery", "../../etc"))
        .header(Header::new("Linear-Signature", sig))
        .body(body)
        .dispatch()
        .await
        .status();
    assert_eq!(status, Status::BadRequest);

    assert!(captured.lock().unwrap().is_empty());
}

#[rocket::async_test]
async fn discord_failure_is_502() {
    let (client, captured) = client("500 Internal Server Error", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);
    assert_eq!(
        post(&client, "/team", body, Some(sig)).await,
        Status::BadGateway
    );
    assert_eq!(captured.lock().unwrap().len(), 1);
}

#[rocket::async_test]
async fn ignore_secret_skips_auth() {
    let (client, captured) = client("204 No Content", true).await;
    // No signature, ancient timestamp: accepted in dangerous mode.
    assert_eq!(
        post(&client, "/team", body_with_timestamp(1), None).await,
        Status::Ok
    );
    assert_eq!(captured.lock().unwrap().len(), 1);
}

/// Marker text that only appears on the static page.
const PAGE_MARKER: &str = "Linear regression";

async fn assert_page(response: rocket::local::asynchronous::LocalResponse<'_>, status: Status) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.content_type(),
        Some(rocket::http::ContentType::HTML)
    );
    let csp = response
        .headers()
        .get_one("Content-Security-Policy")
        .unwrap_or_default()
        .to_owned();
    assert!(csp.contains("default-src 'none'"), "{csp}");
    let body = response.into_string().await.unwrap();
    assert!(body.contains(PAGE_MARKER), "{body}");
}

#[rocket::async_test]
async fn browsing_serves_static_page() {
    let (client, captured) = client("204 No Content", false).await;
    for path in ["/", "/team", "/some/deep/path?x=1", "/nope"] {
        assert_page(client.get(path).dispatch().await, Status::Ok).await;
    }
    let head = client.head("/").dispatch().await;
    assert_eq!(head.status(), Status::Ok);
    assert!(captured.lock().unwrap().is_empty());
}

#[rocket::async_test]
async fn rejected_requests_serve_static_page_with_status() {
    let (client, captured) = client("204 No Content", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);

    let r = client
        .post("/nope")
        .header(Header::new("Linear-Signature", sig.clone()))
        .body(body.clone())
        .dispatch()
        .await;
    assert_page(r, Status::NotFound).await;
    let r = client.post("/team").body(body.clone()).dispatch().await;
    assert_page(r, Status::Unauthorized).await;
    let junk = b"{}".to_vec();
    let r = client
        .post("/team")
        .header(Header::new("Linear-Signature", sign(&junk)))
        .body(junk)
        .dispatch()
        .await;
    assert_page(r, Status::BadRequest).await;
    let r = client.put("/team").body(body.clone()).dispatch().await;
    assert_page(r, Status::NotFound).await;
    let r = client.post("/a/b").body(body).dispatch().await;
    assert_page(r, Status::NotFound).await;

    assert!(captured.lock().unwrap().is_empty());
}

#[rocket::async_test]
async fn linear_responses_stay_plain() {
    let (ok_client, _) = client("204 No Content", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);
    let r = ok_client
        .post("/team")
        .header(Header::new("Linear-Signature", sig))
        .body(body)
        .dispatch()
        .await;
    assert_eq!(r.status(), Status::Ok);
    assert_eq!(r.into_string().await.unwrap(), "ok\n");

    let (failing, _) = client("500 Internal Server Error", false).await;
    let body = body_with_timestamp(now_ms());
    let sig = sign(&body);
    let r = failing
        .post("/team")
        .header(Header::new("Linear-Signature", sig))
        .body(body)
        .dispatch()
        .await;
    assert_eq!(r.status(), Status::BadGateway);
    assert!(!r.into_string().await.unwrap().contains(PAGE_MARKER));
}

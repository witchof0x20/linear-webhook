//! A fake Discord shared by the integration tests.

use std::sync::{Arc, Mutex};

use rocket::tokio::io::{AsyncReadExt, AsyncWriteExt};
use rocket::tokio::net::TcpListener;

/// A captured request to the fake Discord: (request line, body).
pub type Captured = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// Minimal HTTP/1.1 server answering every request with `status` and, if
/// non-empty, a JSON `body`. Returns its base URL and the requests it saw.
pub async fn fake_discord(status: &'static str, body: &'static str) -> (String, Captured) {
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
            let content_type = if body.is_empty() {
                ""
            } else {
                "Content-Type: application/json\r\n"
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });
    (base, captured)
}

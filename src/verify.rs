//! Authentication of incoming Linear webhook deliveries.
//!
//! Linear signs the raw request body with HMAC-SHA256 using the webhook's
//! signing secret and sends the hex digest in the `Linear-Signature` header.
//! The body also carries `webhookTimestamp` (Unix ms), which must be close to
//! the receive time to defeat replays.

use std::fmt;

/// Maximum allowed distance between `webhookTimestamp` and the receive time.
pub const TIMESTAMP_WINDOW_MS: u64 = 60_000;

/// A Linear webhook signing secret.
#[derive(Clone)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self(secret.into())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("missing Linear-Signature header")]
    Missing,
    #[error("Linear-Signature header is not a hex-encoded SHA-256 digest")]
    Malformed,
    #[error("Linear-Signature does not match the request body")]
    Mismatch,
}

/// Check `header` (the raw `Linear-Signature` value) against `body`.
pub fn verify_signature(
    secret: &Secret,
    body: &[u8],
    header: Option<&str>,
) -> Result<(), SignatureError> {
    use hmac::{KeyInit, Mac};

    let header = header.ok_or(SignatureError::Missing)?;
    // Full recognition of the header before use: exactly 64 hex digits.
    if header.len() != 64 {
        return Err(SignatureError::Malformed);
    }
    let expected = hex::decode(header).map_err(|_| SignatureError::Malformed)?;

    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&secret.0)
        .expect("HMAC accepts keys of any length");
    mac.update(body);
    // `verify_slice` compares in constant time.
    mac.verify_slice(&expected)
        .map_err(|_| SignatureError::Mismatch)
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "webhookTimestamp {timestamp_ms} is {skew_ms} ms away from now (allowed: {TIMESTAMP_WINDOW_MS} ms)"
)]
pub struct StaleTimestamp {
    pub timestamp_ms: u64,
    pub skew_ms: u64,
}

/// Reject deliveries whose `webhookTimestamp` is outside the replay window.
pub fn check_timestamp(timestamp_ms: u64, now_ms: u64) -> Result<(), StaleTimestamp> {
    let skew_ms = timestamp_ms.abs_diff(now_ms);
    if skew_ms <= TIMESTAMP_WINDOW_MS {
        Ok(())
    } else {
        Err(StaleTimestamp {
            timestamp_ms,
            skew_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference digest computed with:
    //   printf '%s' '{"a":1}' | openssl dgst -sha256 -hmac 'lin_wh_test' -hex
    const BODY: &[u8] = br#"{"a":1}"#;
    const SECRET: &str = "lin_wh_test";

    fn reference_sig() -> String {
        use hmac::{KeyInit, Mac};
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(BODY);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn accepts_valid_signature() {
        let sig = reference_sig();
        assert_eq!(
            verify_signature(&Secret::new(SECRET), BODY, Some(&sig)),
            Ok(())
        );
    }

    #[test]
    fn accepts_uppercase_hex() {
        let sig = reference_sig().to_uppercase();
        assert_eq!(
            verify_signature(&Secret::new(SECRET), BODY, Some(&sig)),
            Ok(())
        );
    }

    #[test]
    fn rejects_missing_header() {
        assert_eq!(
            verify_signature(&Secret::new(SECRET), BODY, None),
            Err(SignatureError::Missing)
        );
    }

    #[test]
    fn rejects_non_hex_and_wrong_length() {
        let s = Secret::new(SECRET);
        assert_eq!(
            verify_signature(&s, BODY, Some("zz")),
            Err(SignatureError::Malformed)
        );
        assert_eq!(
            verify_signature(&s, BODY, Some("abcd")),
            Err(SignatureError::Malformed)
        );
        let long = format!("{}00", reference_sig());
        assert_eq!(
            verify_signature(&s, BODY, Some(&long)),
            Err(SignatureError::Malformed)
        );
    }

    #[test]
    fn rejects_wrong_secret_or_body() {
        let sig = reference_sig();
        assert_eq!(
            verify_signature(&Secret::new("other"), BODY, Some(&sig)),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_signature(&Secret::new(SECRET), br#"{"a":2}"#, Some(&sig)),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn timestamp_window() {
        let now = 1_700_000_000_000;
        assert!(check_timestamp(now, now).is_ok());
        assert!(check_timestamp(now - TIMESTAMP_WINDOW_MS, now).is_ok());
        assert!(check_timestamp(now + TIMESTAMP_WINDOW_MS, now).is_ok());
        assert_eq!(
            check_timestamp(now - TIMESTAMP_WINDOW_MS - 1, now),
            Err(StaleTimestamp {
                timestamp_ms: now - TIMESTAMP_WINDOW_MS - 1,
                skew_ms: TIMESTAMP_WINDOW_MS + 1
            })
        );
        assert!(check_timestamp(now + TIMESTAMP_WINDOW_MS + 1, now).is_err());
        assert!(check_timestamp(0, now).is_err());
        assert!(check_timestamp(u64::MAX, now).is_err());
    }
}

//! Recognition of Linear webhook payloads.
//!
//! The body is parsed into [`Payload`] and validated before anything else
//! touches it. `data`, `updatedFrom` and `issueData` stay as JSON objects:
//! their shape depends on the entity type, and they are only used for display.

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub id: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub avatar_url: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub action: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub webhook_timestamp: u64,
    pub actor: Option<Actor>,
    pub created_at: Option<String>,
    pub url: Option<String>,
    pub data: Option<Map<String, Value>>,
    pub updated_from: Option<Map<String, Value>>,
    pub organization_id: Option<String>,
    pub webhook_id: Option<String>,
    pub issue_data: Option<Map<String, Value>>,
    pub oauth_client_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("body is not a valid Linear webhook payload: {0}")]
    Json(#[from] serde_json::Error),
    #[error("`{field}` must be 1-64 ASCII alphanumeric characters")]
    BadIdentifier { field: &'static str },
    #[error("`url` is not an http(s) URL")]
    BadUrl,
}

/// `action` and `type` end up in titles and filenames, so they are held to a
/// strict alphabet (Linear uses e.g. `create`, `highRisk`, `Issue`, `IssueSLA`).
fn is_identifier(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Parse a string as an absolute http(s) URL.
pub fn http_url(s: &str) -> Option<url::Url> {
    url::Url::parse(s)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.has_host())
}

impl Payload {
    pub fn parse(body: &[u8]) -> Result<Self, PayloadError> {
        let payload: Payload = serde_json::from_slice(body)?;
        if !is_identifier(&payload.action) {
            return Err(PayloadError::BadIdentifier { field: "action" });
        }
        if !is_identifier(&payload.kind) {
            return Err(PayloadError::BadIdentifier { field: "type" });
        }
        if let Some(url) = &payload.url
            && http_url(url).is_none()
        {
            return Err(PayloadError::BadUrl);
        }
        Ok(payload)
    }

    /// The object describing the subject entity.
    pub fn subject(&self) -> Option<&Map<String, Value>> {
        self.data.as_ref().or(self.issue_data.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_issue_fixture() {
        let p = Payload::parse(include_bytes!("../examples/issue-update.json")).unwrap();
        assert_eq!(p.action, "update");
        assert_eq!(p.kind, "Issue");
        assert_eq!(p.actor.unwrap().name.as_deref(), Some("Ada Lovelace"));
        assert!(p.updated_from.unwrap().contains_key("stateId"));
    }

    #[test]
    fn requires_core_fields() {
        assert!(Payload::parse(br#"{"type":"Issue","webhookTimestamp":1}"#).is_err());
        assert!(Payload::parse(br#"{"action":"create","webhookTimestamp":1}"#).is_err());
        assert!(Payload::parse(br#"{"action":"create","type":"Issue"}"#).is_err());
        assert!(
            Payload::parse(br#"{"action":"create","type":"Issue","webhookTimestamp":-1}"#).is_err()
        );
        assert!(Payload::parse(br#"[]"#).is_err());
        assert!(Payload::parse(b"not json").is_err());
    }

    #[test]
    fn rejects_bad_identifiers_and_urls() {
        assert!(matches!(
            Payload::parse(br#"{"action":"cre ate","type":"Issue","webhookTimestamp":1}"#),
            Err(PayloadError::BadIdentifier { field: "action" })
        ));
        assert!(matches!(
            Payload::parse(br#"{"action":"create","type":"../x","webhookTimestamp":1}"#),
            Err(PayloadError::BadIdentifier { field: "type" })
        ));
        assert!(matches!(
            Payload::parse(br#"{"action":"create","type":"Issue","webhookTimestamp":1,"url":"javascript:alert(1)"}"#),
            Err(PayloadError::BadUrl)
        ));
    }

    #[test]
    fn nulls_are_none() {
        let p = Payload::parse(br#"{"action":"remove","type":"Comment","webhookTimestamp":1,"actor":null,"data":null}"#).unwrap();
        assert!(p.actor.is_none() && p.data.is_none());
    }
}

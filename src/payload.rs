//! Recognition of Linear webhook payloads.
//!
//! The body is parsed into [`Payload`] and validated before anything else
//! touches it. `data`, `updatedFrom` and `issueData` stay as JSON objects:
//! their shape depends on the entity type, and they are only used for display.

use serde::Deserialize;
use serde::de::{Deserializer, Error as _};
use serde_json::{Map, Value};

/// An absolute http(s) URL that is also safe as a Markdown link destination.
///
/// The URL is normalized by the WHATWG parser, which percent-encodes
/// whitespace; parentheses, which it leaves alone, are encoded here. So the
/// serialized form can never end a `[text](url)` link early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl(url::Url);

impl HttpUrl {
    pub fn parse(s: &str) -> Option<Self> {
        let u = url::Url::parse(s).ok()?;
        if !matches!(u.scheme(), "http" | "https") || !u.has_host() {
            return None;
        }
        let escaped = u.as_str().replace('(', "%28").replace(')', "%29");
        url::Url::parse(&escaped).ok().map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl<'de> Deserialize<'de> for HttpUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| D::Error::custom("not an absolute http(s) URL"))
    }
}

/// Display-only URLs: a string that isn't an http(s) URL is dropped rather
/// than failing the whole delivery.
fn lenient_url<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HttpUrl>, D::Error> {
    Ok(Option::<String>::deserialize(d)?
        .as_deref()
        .and_then(HttpUrl::parse))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub id: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    #[serde(default, deserialize_with = "lenient_url")]
    pub url: Option<HttpUrl>,
    #[serde(default, deserialize_with = "lenient_url")]
    pub avatar_url: Option<HttpUrl>,
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
    pub url: Option<HttpUrl>,
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
    #[error("`{field}` must be 1-64 characters of [A-Za-z0-9-]")]
    BadId { field: &'static str },
}

/// `action` and `type` end up in titles and filenames, so they are held to a
/// strict alphabet (Linear uses e.g. `create`, `highRisk`, `Issue`, `IssueSLA`).
fn is_identifier(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Linear's ids (organization, webhook, delivery) are UUIDs. They end up in
/// the footer and attachment name, so hold them to a UUID-like alphabet.
pub fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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
        for (field, value) in [
            ("organizationId", &payload.organization_id),
            ("webhookId", &payload.webhook_id),
        ] {
            if value.as_deref().is_some_and(|v| !is_id(v)) {
                return Err(PayloadError::BadId { field });
            }
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
            Err(PayloadError::Json(_))
        ));
    }

    #[test]
    fn http_urls() {
        for bad in [
            "javascript:alert(1)",
            "/relative",
            "mailto:a@b.c",
            "file:///etc",
        ] {
            assert_eq!(HttpUrl::parse(bad), None, "{bad}");
        }
        let u = HttpUrl::parse(" https://linear.app/a b)(c?d)#e( ").unwrap();
        assert_eq!(u.as_str(), "https://linear.app/a%20b%29%28c?d%29#e%28");
    }

    /// The actor's links are display-only: bad ones are dropped, not fatal.
    #[test]
    fn actor_urls_are_lenient() {
        let p = Payload::parse(
            br#"{"action":"create","type":"Issue","webhookTimestamp":1,
                "actor":{"url":"javascript:x","avatarUrl":"https://a.example/i.png"}}"#,
        )
        .unwrap();
        let actor = p.actor.unwrap();
        assert_eq!(actor.url, None);
        assert_eq!(
            actor.avatar_url.as_ref().map(HttpUrl::as_str),
            Some("https://a.example/i.png")
        );
    }

    /// Linear's ids are UUIDs; they end up in the footer.
    #[test]
    fn bounds_ids() {
        for field in ["organizationId", "webhookId"] {
            for bad in ["a b".to_owned(), "x".repeat(65), String::new()] {
                let body = serde_json::to_vec(&serde_json::json!({
                    "action": "create", "type": "Issue", "webhookTimestamp": 1, field: bad,
                }))
                .unwrap();
                assert!(
                    matches!(Payload::parse(&body), Err(PayloadError::BadId { .. })),
                    "{field}={bad:?}"
                );
            }
        }
        let ok = br#"{"action":"create","type":"Issue","webhookTimestamp":1,
            "organizationId":"d3b7c1e0-0000-4000-8000-000000000001"}"#;
        assert!(Payload::parse(ok).is_ok());
    }

    #[test]
    fn nulls_are_none() {
        let p = Payload::parse(br#"{"action":"remove","type":"Comment","webhookTimestamp":1,"actor":null,"data":null}"#).unwrap();
        assert!(p.actor.is_none() && p.data.is_none());
    }
}

//! Turn a Linear payload into a Discord message.
//!
//! The embed is a readable summary. The full, unmodified request body is
//! attached zstd-compressed, so nothing is lost when the embed has to be cut
//! down to fit Discord's limits.

use serde_json::{Map, Value};
use serenity::model::Timestamp;

use crate::payload::{HttpUrl, Payload};

// Discord embed limits (https://discord.com/developers/docs/resources/message#embed-object-embed-limits).
pub const TITLE_MAX: usize = 256;
pub const DESCRIPTION_MAX: usize = 4096;
pub const FIELD_NAME_MAX: usize = 256;
pub const FIELD_VALUE_MAX: usize = 1024;
pub const FOOTER_MAX: usize = 2048;
pub const AUTHOR_MAX: usize = 256;
pub const FIELDS_MAX: usize = 25;
pub const TOTAL_MAX: usize = 6000;

/// Our own, smaller cap so a long issue description doesn't crowd out the fields.
const DESCRIPTION_SOFT_MAX: usize = 1500;
const _: () = assert!(DESCRIPTION_SOFT_MAX <= DESCRIPTION_MAX);
/// Cap for a single value shown inside the "Changes" field.
const CHANGE_VALUE_MAX: usize = 120;
const ZSTD_LEVEL: i32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Author {
    pub name: String,
    pub url: Option<HttpUrl>,
    pub icon_url: Option<HttpUrl>,
}

/// A Discord embed that is guaranteed to be within Discord's limits once
/// built by [`render`].
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedSpec {
    pub title: String,
    pub url: Option<HttpUrl>,
    pub description: Option<String>,
    pub color: u32,
    pub author: Option<Author>,
    pub fields: Vec<Field>,
    pub footer: String,
    pub timestamp: Option<Timestamp>,
}

impl EmbedSpec {
    pub fn total_len(&self) -> usize {
        let n = |s: &str| s.chars().count();
        n(&self.title)
            + self.description.as_deref().map_or(0, n)
            + self.author.as_ref().map_or(0, |a| n(&a.name))
            + n(&self.footer)
            + self
                .fields
                .iter()
                .map(|f| n(&f.name) + n(&f.value))
                .sum::<usize>()
    }
}

#[derive(Debug)]
pub struct Rendered {
    pub embed: EmbedSpec,
    pub filename: String,
    pub attachment: Vec<u8>,
}

/// Truncate to at most `max` characters, marking the cut with `…`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escape characters that would break out of Discord markdown link text.
fn escape_link_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// Make `s` render as literal text: control characters (including newlines)
/// become spaces and all ASCII punctuation is backslash-escaped.
fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            out.push(' ');
            continue;
        }
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// "ProjectUpdate" -> "Project update", "IssueSLA" -> "Issue SLA".
fn humanize_kind(kind: &str) -> String {
    // "OAuth" defeats the camel-case heuristic below.
    if let Some(rest) = kind.strip_prefix("OAuth") {
        return match rest {
            "" => "OAuth".into(),
            rest => format!("OAuth {}", humanize_kind(rest).to_lowercase()),
        };
    }
    let mut out = String::new();
    let chars: Vec<char> = kind.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_ascii_uppercase()
            && (chars[i - 1].is_ascii_lowercase()
                || chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase()));
        if boundary {
            out.push(' ');
            // Lower-case the start of a new word unless it is part of an acronym.
            if chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase()) {
                out.push(c.to_ascii_lowercase());
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn action_phrase(action: &str) -> &str {
    match action {
        "create" => "created",
        "update" => "updated",
        "remove" => "removed",
        "highRisk" => "at high risk",
        other => other,
    }
}

fn action_color(action: &str) -> u32 {
    match action {
        "create" => 0x2ecc71,
        "update" => 0x5e6ad2,
        "remove" | "breached" | "revoked" => 0xe74c3c,
        "highRisk" => 0xe67e22,
        _ => 0x95a5a6,
    }
}

fn str_of<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Short human label for an entity object, e.g. "ENG-42 Fix bug" or "In Progress".
fn label_of(obj: &Map<String, Value>) -> Option<String> {
    if let (Some(id), Some(title)) = (str_of(obj, "identifier"), str_of(obj, "title")) {
        return Some(format!("{id} {title}"));
    }
    for key in ["name", "displayName", "title", "identifier", "key", "emoji"] {
        if let Some(s) = str_of(obj, key) {
            return Some(s.to_owned());
        }
    }
    if let Some(n) = obj.get("number").and_then(Value::as_u64) {
        return Some(format!("#{n}"));
    }
    str_of(obj, "body").map(|b| truncate(b.lines().next().unwrap_or(b), 80))
}

/// Render any JSON value as compact display text. `None` means "nothing worth showing".
fn display(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { "yes" } else { "no" }.into()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(display).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::Object(obj) => match label_of(obj) {
            Some(label) => Some(match str_of(obj, "url").and_then(HttpUrl::parse) {
                Some(url) => format!("[{}]({})", escape_link_text(&label), url.as_str()),
                None => label,
            }),
            None => str_of(obj, "id").map(str::to_owned),
        },
    }
}

/// Fields shown when present on the subject entity, in display order.
const CURATED: &[(&str, &str)] = &[
    ("state", "State"),
    ("status", "Status"),
    ("priorityLabel", "Priority"),
    ("team", "Team"),
    ("assignee", "Assignee"),
    ("creator", "Creator"),
    ("user", "User"),
    ("lead", "Lead"),
    ("owner", "Owner"),
    ("issue", "Issue"),
    ("parent", "Parent"),
    ("project", "Project"),
    ("projectMilestone", "Milestone"),
    ("cycle", "Cycle"),
    ("initiative", "Initiative"),
    ("customer", "Customer"),
    ("labels", "Labels"),
    ("estimate", "Estimate"),
    ("health", "Health"),
    ("startDate", "Start date"),
    ("targetDate", "Target date"),
    ("dueDate", "Due date"),
    ("startsAt", "Starts"),
    ("endsAt", "Ends"),
    ("emoji", "Emoji"),
    ("color", "Color"),
    ("email", "Email"),
    ("domains", "Domains"),
    ("subtitle", "Subtitle"),
];

fn priority_name(p: u64) -> Option<&'static str> {
    ["No priority", "Urgent", "High", "Medium", "Low"]
        .get(p as usize)
        .copied()
}

fn curated_fields(subject: &Map<String, Value>) -> Vec<Field> {
    let mut fields = Vec::new();
    for &(key, name) in CURATED {
        let value = match key {
            // Fall back to the numeric priority when no label was sent.
            "priorityLabel" => display(subject.get(key).unwrap_or(&Value::Null)).or_else(|| {
                subject
                    .get("priority")
                    .and_then(Value::as_u64)
                    .and_then(priority_name)
                    .map(Into::into)
            }),
            _ => subject.get(key).and_then(display),
        };
        if let Some(value) = value {
            fields.push(Field {
                name: name.into(),
                value,
                inline: true,
            });
        }
    }
    fields
}

/// Display text for the current value of a changed property. Relations are
/// reported as `fooId` / `fooIds`, so prefer the expanded `foo` / `foos` object.
fn current_value(subject: &Map<String, Value>, key: &str) -> Option<String> {
    let expanded = key
        .strip_suffix("Ids")
        .map(|base| format!("{base}s"))
        .or_else(|| key.strip_suffix("Id").map(str::to_owned));
    if let Some(v) = expanded.and_then(|k| subject.get(&k)).and_then(display) {
        return Some(v);
    }
    subject.get(key).and_then(display)
}

fn changes_field(
    subject: Option<&Map<String, Value>>,
    updated_from: &Map<String, Value>,
) -> Option<Field> {
    let empty = Map::new();
    let subject = subject.unwrap_or(&empty);
    let mut lines = Vec::new();
    for (key, old) in updated_from {
        if key == "updatedAt" {
            continue;
        }
        let old = display(old).unwrap_or_else(|| "none".into());
        let new = current_value(subject, key).unwrap_or_else(|| "none".into());
        let (old, new) = (
            truncate(&old, CHANGE_VALUE_MAX),
            truncate(&new, CHANGE_VALUE_MAX),
        );
        lines.push(format!("**{}**: {old} → {new}", escape_markdown(key)));
    }
    if lines.is_empty() {
        return None;
    }
    // Keep whole lines when possible; note how many didn't fit.
    let mut value = String::new();
    for (i, line) in lines.iter().enumerate() {
        let remaining = lines.len() - i;
        let more = format!("\n…and {remaining} more");
        let candidate = if value.is_empty() {
            line.clone()
        } else {
            format!("{value}\n{line}")
        };
        if candidate.chars().count() + more.chars().count() > FIELD_VALUE_MAX {
            value.push_str(if value.is_empty() { &more[1..] } else { &more });
            break;
        }
        value = candidate;
    }
    Some(Field {
        name: "Changes".into(),
        value,
        inline: false,
    })
}

fn headline(subject: Option<&Map<String, Value>>) -> Option<String> {
    subject.and_then(label_of)
}

fn description(subject: Option<&Map<String, Value>>) -> Option<String> {
    let subject = subject?;
    ["description", "body", "content"]
        .into_iter()
        .find_map(|k| str_of(subject, k))
        .map(|s| truncate(s, DESCRIPTION_SOFT_MAX))
}

fn author(payload: &Payload) -> Option<Author> {
    let actor = payload.actor.as_ref()?;
    let name = actor
        .name
        .as_deref()
        .or(actor.email.as_deref())
        .or(actor.id.as_deref())?;
    let name = match actor.kind.as_deref() {
        Some(kind) if kind != "user" => format!("{name} ({kind})"),
        _ => name.to_owned(),
    };
    Some(Author {
        name: truncate(&name, AUTHOR_MAX),
        url: actor.url.clone(),
        icon_url: actor.avatar_url.clone(),
    })
}

fn footer(payload: &Payload, delivery: Option<&str>) -> String {
    let mut parts = vec!["Linear".to_owned()];
    if let Some(org) = &payload.organization_id {
        parts.push(format!("org {org}"));
    }
    if let Some(id) = &payload.webhook_id {
        parts.push(format!("webhook {id}"));
    }
    if let Some(d) = delivery {
        parts.push(format!("delivery {d}"));
    }
    truncate(&parts.join(" · "), FOOTER_MAX)
}

fn truncated_note(dropped: usize) -> Field {
    Field {
        name: "Truncated".into(),
        value: format!("{dropped} field(s) omitted; see attachment"),
        inline: false,
    }
}

/// Bring the embed under Discord's total size limit by dropping trailing
/// fields first, then shortening the description. `dropped` counts fields
/// already removed to respect the field-count limit.
///
/// Without fields, the fixed parts (title, author, footer, note) add up to
/// well under [`TOTAL_MAX`], so shrinking the description always suffices.
fn enforce_total(embed: &mut EmbedSpec, mut dropped: usize) {
    let note_len = |dropped: usize| {
        let note = truncated_note(dropped);
        note.name.chars().count() + note.value.chars().count()
    };
    // Account for the note before deciding whether a field fits.
    let fits = |embed: &EmbedSpec, dropped: usize| {
        let note = if dropped > 0 { note_len(dropped) } else { 0 };
        embed.total_len() + note <= TOTAL_MAX
    };
    while !fits(embed, dropped) && !embed.fields.is_empty() {
        embed.fields.pop();
        dropped += 1;
    }
    if dropped > 0 {
        embed.fields.push(truncated_note(dropped));
    }
    let over = embed.total_len().saturating_sub(TOTAL_MAX);
    if over > 0
        && let Some(desc) = &embed.description
    {
        let keep = desc.chars().count().saturating_sub(over);
        embed.description = Some(truncate(desc, keep)).filter(|d| !d.is_empty());
    }
}

pub fn render(payload: &Payload, raw_body: &[u8], delivery: Option<&str>) -> Rendered {
    let subject = payload.subject();

    let mut title = format!(
        "{} {}",
        humanize_kind(&payload.kind),
        action_phrase(&payload.action)
    );
    if let Some(h) = headline(subject) {
        title = format!("{title}: {h}");
    }

    let mut fields = Vec::new();
    if let Some(updated_from) = &payload.updated_from {
        fields.extend(changes_field(subject, updated_from));
    }
    if let Some(subject) = subject {
        fields.extend(curated_fields(subject));
    }
    if let Some(client) = &payload.oauth_client_id {
        fields.push(Field {
            name: "OAuth client".into(),
            value: client.clone(),
            inline: true,
        });
    }
    for f in &mut fields {
        f.name = truncate(&f.name, FIELD_NAME_MAX);
        f.value = truncate(&f.value, FIELD_VALUE_MAX);
    }
    // Leave room for the "Truncated" note.
    let over_count = fields.len().saturating_sub(FIELDS_MAX - 1);
    fields.truncate(FIELDS_MAX - 1);

    let mut embed = EmbedSpec {
        title: truncate(&title, TITLE_MAX),
        url: payload.url.clone(),
        description: description(subject),
        color: action_color(&payload.action),
        author: author(payload),
        fields,
        footer: footer(payload, delivery),
        timestamp: payload
            .created_at
            .as_deref()
            .and_then(|t| Timestamp::parse(t).ok()),
    };
    enforce_total(&mut embed, over_count);

    let tag = delivery.map_or_else(|| payload.webhook_timestamp.to_string(), str::to_owned);
    let filename = format!("linear-{}-{}-{tag}.json.zst", payload.kind, payload.action);
    let attachment =
        zstd::bulk::compress(raw_body, ZSTD_LEVEL).expect("in-memory zstd compression cannot fail");

    Rendered {
        embed,
        filename,
        attachment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(bytes: &[u8]) -> Rendered {
        render(
            &Payload::parse(bytes).unwrap(),
            bytes,
            Some("d3b7c1e0-0000-4000-8000-000000000001"),
        )
    }

    fn field<'a>(r: &'a Rendered, name: &str) -> Option<&'a str> {
        r.embed
            .fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.as_str())
    }

    #[test]
    fn humanizes_kinds() {
        assert_eq!(humanize_kind("Issue"), "Issue");
        assert_eq!(humanize_kind("ProjectUpdate"), "Project update");
        assert_eq!(humanize_kind("IssueSLA"), "Issue SLA");
        assert_eq!(humanize_kind("OAuthApp"), "OAuth app");
        assert_eq!(humanize_kind("CustomerNeed"), "Customer need");
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("héllo", 10), "héllo");
        assert_eq!(truncate("héllo", 3), "hé…");
        assert_eq!(truncate("🦀🦀🦀", 2).chars().count(), 2);
    }

    #[test]
    fn renders_issue_update() {
        let body = include_bytes!("../examples/issue-update.json");
        let r = fixture(body);
        assert_eq!(
            r.embed.title,
            "Issue updated: ENG-42 Webhook forwarder drops events during Discord outages"
        );
        assert_eq!(
            r.embed.url.as_ref().map(HttpUrl::as_str),
            Some("https://linear.app/example/issue/ENG-42/webhook-forwarder-drops-events")
        );
        assert_eq!(r.embed.author.as_ref().unwrap().name, "Ada Lovelace");
        assert_eq!(field(&r, "State"), Some("In Progress"));
        assert_eq!(field(&r, "Priority"), Some("High"));
        assert_eq!(field(&r, "Team"), Some("Engineering"));
        assert_eq!(field(&r, "Labels"), Some("Bug, Infra"));
        assert_eq!(
            field(&r, "Project"),
            Some("[Notifications](https://linear.app/example/project/notifications)")
        );
        assert_eq!(field(&r, "Cycle"), Some("#12"));
        let changes = field(&r, "Changes").unwrap();
        assert!(
            changes.contains("**stateId**: aaaa-todo → In Progress"),
            "{changes}"
        );
        assert!(changes.contains("**priority**: 3 → 2"), "{changes}");
        assert!(
            changes.contains("**assigneeId**: none → [Ada Lovelace]"),
            "{changes}"
        );
        assert!(!changes.contains("updatedAt"));
        assert!(
            r.embed
                .description
                .as_ref()
                .unwrap()
                .contains("return `502`")
        );
        assert!(r.embed.footer.contains("delivery d3b7c1e0"));
        assert!(r.embed.timestamp.is_some());
        assert_eq!(
            r.filename,
            "linear-Issue-update-d3b7c1e0-0000-4000-8000-000000000001.json.zst"
        );
        // The attachment round-trips to the exact bytes Linear sent.
        assert_eq!(zstd::decode_all(&r.attachment[..]).unwrap(), body);
    }

    #[test]
    fn renders_comment_and_sla() {
        let c = fixture(include_bytes!("../examples/comment-create.json"));
        assert!(
            c.embed
                .title
                .starts_with("Comment created: Repro'd locally."),
            "{}",
            c.embed.title
        );
        assert_eq!(
            field(&c, "Issue"),
            Some(
                "[ENG-42 Webhook forwarder drops events during Discord outages](https://linear.app/example/issue/ENG-42)"
            )
        );
        assert_eq!(c.embed.color, 0x2ecc71);

        let s = fixture(include_bytes!("../examples/issue-sla-breached.json"));
        assert_eq!(
            s.embed.title,
            "Issue SLA breached: ENG-42 Webhook forwarder drops events during Discord outages"
        );
        assert_eq!(field(&s, "Priority"), Some("Urgent"));
        assert!(s.embed.author.is_none());
    }

    #[test]
    fn minimal_payload() {
        let body =
            br#"{"action":"revoked","type":"OAuthApp","webhookTimestamp":5,"oauthClientId":"abc"}"#;
        let r = render(&Payload::parse(body).unwrap(), body, None);
        assert_eq!(r.embed.title, "OAuth app revoked");
        assert_eq!(field(&r, "OAuth client"), Some("abc"));
        assert_eq!(r.filename, "linear-OAuthApp-revoked-5.json.zst");
    }

    #[test]
    fn huge_payload_fits_discord_limits() {
        let long = "x".repeat(10_000);
        let mut data = serde_json::Map::new();
        data.insert("title".into(), long.clone().into());
        data.insert("description".into(), long.clone().into());
        for (key, _) in CURATED {
            data.insert((*key).into(), long.clone().into());
        }
        let mut updated_from = serde_json::Map::new();
        for i in 0..200 {
            updated_from.insert(format!("field{i}"), long.clone().into());
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "update", "type": "Issue", "webhookTimestamp": 1,
            "actor": {"name": long}, "data": data, "updatedFrom": updated_from,
            "organizationId": "o".repeat(64), "webhookId": "w".repeat(64),
        }))
        .unwrap();
        let r = render(&Payload::parse(&body).unwrap(), &body, None);
        assert_within_limits(&r.embed);
        assert!(field(&r, "Changes").unwrap().contains("more"));
        assert!(field(&r, "Truncated").is_some());
    }

    fn assert_within_limits(e: &EmbedSpec) {
        assert!(e.total_len() <= TOTAL_MAX, "total {}", e.total_len());
        assert!(e.fields.len() <= FIELDS_MAX, "fields {}", e.fields.len());
        assert!(e.title.chars().count() <= TITLE_MAX);
        assert!(e.footer.chars().count() <= FOOTER_MAX);
        assert!(
            e.description
                .as_deref()
                .is_none_or(|d| d.chars().count() <= DESCRIPTION_MAX)
        );
        assert!(
            e.author
                .as_ref()
                .is_none_or(|a| a.name.chars().count() <= AUTHOR_MAX)
        );
        for f in &e.fields {
            assert!(f.name.chars().count() <= FIELD_NAME_MAX);
            assert!(f.value.chars().count() <= FIELD_VALUE_MAX);
        }
    }

    /// Without a description to shrink, the "Truncated" note itself must
    /// still fit under the total limit.
    #[test]
    fn note_fits_without_description() {
        let long = "x".repeat(2000);
        for n in 0..1200 {
            // The first field is kept and varies in length, so for some `n`
            // the kept fields end just under the limit, leaving no room for
            // the note unless one more field is dropped.
            let mut data = serde_json::Map::new();
            data.insert("state".into(), "s".repeat(n).into());
            for k in ["status", "team", "assignee", "creator", "user", "lead"] {
                data.insert(k.into(), long.clone().into());
            }
            let body = serde_json::to_vec(&serde_json::json!({
                "action": "update", "type": "Issue", "webhookTimestamp": 1, "data": data,
            }))
            .unwrap();
            let r = render(&Payload::parse(&body).unwrap(), &body, None);
            assert_within_limits(&r.embed);
            assert!(field(&r, "Truncated").is_some());
        }
    }

    /// Fields beyond Discord's count limit are reported, not silently lost.
    #[test]
    fn too_many_fields_are_reported() {
        let mut data = serde_json::Map::new();
        for (key, _) in CURATED {
            data.insert((*key).into(), "v".into());
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "update", "type": "Issue", "webhookTimestamp": 1, "data": data,
        }))
        .unwrap();
        let r = render(&Payload::parse(&body).unwrap(), &body, None);
        assert_within_limits(&r.embed);
        let omitted = CURATED.len() - (FIELDS_MAX - 1);
        assert_eq!(
            field(&r, "Truncated"),
            Some(format!("{omitted} field(s) omitted; see attachment").as_str())
        );
    }

    /// A URL that passes the http(s) check must still not be able to close
    /// the Markdown link early and start its own.
    #[test]
    fn link_destination_cannot_break_out() {
        let v = serde_json::json!({
            "name": "n",
            "url": "https://linear.app/x) [click](https://evil.example/ok",
        });
        let out = display(&v).unwrap();
        assert!(out.starts_with("[n](https://linear.app/x"), "{out}");
        assert_eq!(out.matches(')').count(), 1, "{out}");
        assert!(out.ends_with(')'), "{out}");
        assert!(!out.contains(' '), "{out}");
    }

    /// `updatedFrom` keys are shown in bold; they must not inject Markdown.
    #[test]
    fn change_keys_are_escaped() {
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "update", "type": "Issue", "webhookTimestamp": 1,
            "data": {}, "updatedFrom": {"a**\n# [x](https://evil.example)": 1},
        }))
        .unwrap();
        let r = render(&Payload::parse(&body).unwrap(), &body, None);
        let changes = field(&r, "Changes").unwrap();
        assert_eq!(
            changes,
            r"**a\*\* \# \[x\]\(https\:\/\/evil\.example\)**: 1 → none"
        );
    }

    #[test]
    fn link_text_is_escaped_and_bad_urls_unlinked() {
        let v = serde_json::json!({"name": "a](https://evil)", "url": "https://linear.app/x"});
        assert_eq!(
            display(&v).unwrap(),
            "[a\\](https://evil)](https://linear.app/x)"
        );
        let v = serde_json::json!({"name": "n", "url": "javascript:alert(1)"});
        assert_eq!(display(&v).unwrap(), "n");
    }
}

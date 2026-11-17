//! Property test: whatever Linear sends, the rendered embed stays within
//! Discord's limits. A violation makes Discord reject the message, which
//! Linear would then retry forever.

use proptest::prelude::*;
use serde_json::{Map, Value, json};

use linear_webhook::payload::Payload;
use linear_webhook::render::{
    AUTHOR_MAX, DESCRIPTION_MAX, EmbedSpec, FIELD_NAME_MAX, FIELD_VALUE_MAX, FIELDS_MAX,
    FOOTER_MAX, TITLE_MAX, TOTAL_MAX, render,
};

/// Keys the renderer looks at on the subject entity.
const SUBJECT_KEYS: &[&str] = &[
    "title",
    "identifier",
    "name",
    "description",
    "body",
    "state",
    "status",
    "priorityLabel",
    "priority",
    "team",
    "assignee",
    "creator",
    "user",
    "lead",
    "owner",
    "issue",
    "parent",
    "project",
    "projectMilestone",
    "cycle",
    "initiative",
    "customer",
    "labels",
    "estimate",
    "health",
    "startDate",
    "targetDate",
    "dueDate",
    "startsAt",
    "endsAt",
    "emoji",
    "color",
    "email",
    "domains",
    "subtitle",
];

/// Mostly short strings, sometimes very long ones, with multi-byte characters.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-z🦀é\\[\\]*_ ]{0,40}",
        1 => (0usize..8000, prop::char::any()).prop_map(|(n, c)| c.to_string().repeat(n)),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        text().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::from),
        Just(Value::Null),
    ];
    leaf.prop_recursive(2, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            (text(), text()).prop_map(|(name, url)| json!({
                "name": name,
                "url": format!("https://linear.app/{url}"),
            })),
        ]
    })
}

fn subject() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::vec((prop::sample::select(SUBJECT_KEYS), value()), 0..40)
        .prop_map(|kv| kv.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn updated_from() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::vec(("[a-zA-Z]{1,30}", value()), 0..60)
        .prop_map(|kv| kv.into_iter().collect())
}

fn body() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::option::of(subject()),
        prop::option::of(updated_from()),
        prop::option::of(text()),
        prop::option::of(text()),
        prop::option::of(text()),
        prop::option::of(text()),
    )
        .prop_map(|(data, updated, actor, org, webhook, oauth)| {
            serde_json::to_vec(&json!({
                "action": "update",
                "type": "Issue",
                "webhookTimestamp": 1,
                "data": data,
                "updatedFrom": updated,
                "actor": actor.map(|name| json!({"name": name, "type": "bot"})),
                "organizationId": org,
                "webhookId": webhook,
                "oauthClientId": oauth,
            }))
            .unwrap()
        })
}

fn check(e: &EmbedSpec) -> Result<(), TestCaseError> {
    let n = |s: &str| s.chars().count();
    prop_assert!(e.total_len() <= TOTAL_MAX, "total {}", e.total_len());
    prop_assert!(e.fields.len() <= FIELDS_MAX, "fields {}", e.fields.len());
    prop_assert!(n(&e.title) <= TITLE_MAX);
    prop_assert!(n(&e.footer) <= FOOTER_MAX);
    prop_assert!(
        e.description
            .as_deref()
            .is_none_or(|d| n(d) <= DESCRIPTION_MAX)
    );
    prop_assert!(e.author.as_ref().is_none_or(|a| n(&a.name) <= AUTHOR_MAX));
    for f in &e.fields {
        prop_assert!(n(&f.name) <= FIELD_NAME_MAX);
        prop_assert!(n(&f.value) <= FIELD_VALUE_MAX);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn embed_fits_discord_limits(body in body(), delivery in prop::option::of("[a-f0-9-]{1,64}")) {
        let payload = Payload::parse(&body).unwrap();
        check(&render(&payload, &body, delivery.as_deref()).embed)?;
    }
}

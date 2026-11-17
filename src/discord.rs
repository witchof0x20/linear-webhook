//! Delivering rendered messages to a Discord webhook.

use std::time::Duration;

use serenity::builder::{
    Builder, CreateAllowedMentions, CreateAttachment, CreateEmbed, CreateEmbedAuthor,
    CreateEmbedFooter, ExecuteWebhook,
};
use serenity::http::{Http, HttpBuilder};

use crate::config::DiscordWebhook;
use crate::render::{EmbedSpec, Rendered};

/// Linear gives up after 5 s, so Discord gets a bit less than that.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("Discord did not respond within {0:?}")]
    Timeout(Duration),
    #[error("Discord request failed: {0}")]
    Discord(#[from] serenity::Error),
}

impl SendError {
    /// Whether sending the same message again could succeed: timeouts,
    /// network errors, rate limits and Discord server errors. A rejected
    /// message, unknown webhook or bad token won't fix itself.
    pub fn is_retryable(&self) -> bool {
        use serenity::http::HttpError;
        match self {
            Self::Timeout(_) => true,
            Self::Discord(serenity::Error::Io(_)) => true,
            Self::Discord(serenity::Error::Http(HttpError::Request(_))) => true,
            Self::Discord(serenity::Error::Http(HttpError::UnsuccessfulRequest(r))) => {
                let s = r.status_code;
                !s.is_client_error() || matches!(s.as_u16(), 408 | 429)
            }
            Self::Discord(_) => false,
        }
    }
}

/// Serenity instruments its HTTP calls with `tracing` spans whose fields
/// include the webhook token. With no tracing subscriber installed, those
/// spans fall back to the `log` crate and end up in Rocket's log output.
/// Installing a global no-op subscriber turns that fallback off. Our own
/// logging goes through `log` directly and is unaffected.
fn silence_serenity_tracing() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Fails only if a subscriber is already installed, which also
        // disables the fallback.
        let _ =
            tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default());
    });
}

/// An HTTP client for webhook execution only (no bot token).
///
/// Serenity's ratelimiter is disabled: it would sleep through a 429, blowing
/// Linear's 5 s deadline. A 429 instead becomes an error, the handler answers
/// 502, and Linear retries the delivery later.
///
/// `proxy` replaces `https://discord.com` as the API base; used for testing.
pub fn client(proxy: Option<&str>) -> Http {
    silence_serenity_tracing();
    let mut builder = HttpBuilder::new("").ratelimiter_disabled(true);
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    }
    builder.build()
}

fn to_embed(spec: EmbedSpec) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(spec.title)
        .colour(spec.color)
        .footer(CreateEmbedFooter::new(spec.footer));
    if let Some(url) = spec.url {
        embed = embed.url(url.as_str());
    }
    if let Some(description) = spec.description {
        embed = embed.description(description);
    }
    if let Some(author) = spec.author {
        let mut a = CreateEmbedAuthor::new(author.name);
        if let Some(url) = author.url {
            a = a.url(url.as_str());
        }
        if let Some(icon) = author.icon_url {
            a = a.icon_url(icon.as_str());
        }
        embed = embed.author(a);
    }
    if let Some(ts) = spec.timestamp {
        embed = embed.timestamp(ts);
    }
    embed.fields(spec.fields.into_iter().map(|f| (f.name, f.value, f.inline)))
}

pub async fn send(
    http: &Http,
    webhook: &DiscordWebhook,
    message: Rendered,
) -> Result<(), SendError> {
    let builder = ExecuteWebhook::new()
        .embed(to_embed(message.embed))
        .add_file(CreateAttachment::bytes(
            message.attachment,
            message.filename,
        ))
        // Embeds don't ping, but make sure nothing in the payload ever can.
        .allowed_mentions(CreateAllowedMentions::new());
    // Discord validates the request either way; `wait = false` only skips
    // echoing the created message back.
    rocket::tokio::time::timeout(
        SEND_TIMEOUT,
        builder.execute(http, (webhook.id, &webhook.token, false)),
    )
    .await
    .map_err(|_| SendError::Timeout(SEND_TIMEOUT))??;
    Ok(())
}

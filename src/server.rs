//! The Rocket HTTP server: `POST /<server>` per configured server.

use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use rocket::data::{ByteUnit, ToByteUnit};
use rocket::figment::Figment;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome, Request};
use rocket::{Build, Data, Rocket, State, post, routes};
use serenity::http::Http;

use crate::config::{Auth, Config};
use crate::payload::{Payload, is_id};
use crate::verify::{check_timestamp, verify_signature};
use crate::{discord, page, render};

/// Linear payloads are a few KiB; anything this large is not from Linear.
pub fn body_limit() -> ByteUnit {
    1.mebibytes()
}

pub struct App {
    pub config: Config,
    pub http: Http,
}

/// The Linear headers we use. Recognition happens in the handler so it can
/// pick the status code.
pub struct LinearHeaders<'r> {
    signature: Option<&'r str>,
    delivery: Option<&'r str>,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for LinearHeaders<'r> {
    type Error = Infallible;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let headers = req.headers();
        Outcome::Success(Self {
            signature: headers.get_one("Linear-Signature"),
            delivery: headers.get_one("Linear-Delivery"),
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `Ok` is a plain-text answer for Linear. `Err` rejects the request; Rocket
/// renders it with the static page via [`page::catcher`].
type Reply = Result<(Status, &'static str), Status>;

#[post("/<server>", data = "<body>")]
async fn webhook(
    server: &str,
    headers: LinearHeaders<'_>,
    body: Data<'_>,
    app: &State<App>,
) -> Reply {
    let Some(target) = app.config.servers.get(server) else {
        return Err(Status::NotFound);
    };

    let delivery = match headers.delivery {
        Some(d) if !is_id(d) => {
            return Err(Status::BadRequest);
        }
        d => d,
    };

    let body = match body.open(body_limit()).into_bytes().await {
        Ok(b) if b.is_complete() => b.into_inner(),
        Ok(_) => return Err(Status::PayloadTooLarge),
        Err(e) => {
            log::warn!("[{server}] failed to read body: {e}");
            return Err(Status::BadRequest);
        }
    };

    // Authenticate before parsing anything.
    if let Auth::Verify(secret) = &target.auth
        && let Err(e) = verify_signature(secret, &body, headers.signature)
    {
        log::warn!("[{server}] rejected delivery {delivery:?}: {e}");
        return Err(Status::Unauthorized);
    }

    let payload = match Payload::parse(&body) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[{server}] rejected delivery {delivery:?}: {e}");
            return Err(Status::BadRequest);
        }
    };

    if let Auth::Verify(_) = &target.auth
        && let Err(e) = check_timestamp(payload.webhook_timestamp, now_ms())
    {
        log::warn!("[{server}] rejected delivery {delivery:?}: {e}");
        return Err(Status::Unauthorized);
    }

    let message = render::render(&payload, &body, delivery);
    match discord::send(&app.http, &target.webhook, message).await {
        Ok(()) => {
            log::info!(
                "[{server}] forwarded {} {} (delivery {delivery:?})",
                payload.kind,
                payload.action
            );
            Ok((Status::Ok, "ok\n"))
        }
        Err(e) if e.is_retryable() => {
            log::error!("[{server}] failed to forward delivery {delivery:?}: {e}");
            Ok((Status::BadGateway, "failed to forward to Discord\n"))
        }
        // Acknowledge so Linear doesn't retry (and eventually disable the
        // webhook) over something only the operator can fix.
        Err(e) => {
            log::error!("[{server}] dropped delivery {delivery:?}, Discord rejected it: {e}");
            Ok((Status::Ok, "dropped: Discord rejected the message\n"))
        }
    }
}

pub fn rocket(figment: Figment, app: App) -> Rocket<Build> {
    rocket::custom(figment)
        .manage(app)
        .mount("/", routes![webhook, page::browse])
        .register("/", rocket::catchers![page::catcher])
}

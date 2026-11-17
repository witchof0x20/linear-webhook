//! The static page served for every request that isn't a Linear delivery.

use std::io::Cursor;

use rocket::http::{ContentType, Status};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use rocket::{catch, get};

/// Embedded into the binary at compile time.
pub const HTML: &str = include_str!("../static/linear-regression.html");

/// The page has no scripts, external resources or forms.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; \
                   form-action 'none'; frame-ancestors 'none'";

/// The static page, served with the given status.
pub struct Page(pub Status);

impl<'r> Responder<'r, 'static> for Page {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        Response::build()
            .status(self.0)
            .header(ContentType::HTML)
            .raw_header("Content-Security-Policy", CSP)
            .sized_body(HTML.len(), Cursor::new(HTML))
            .ok()
    }
}

/// Any GET (and, via Rocket's automatic handling, HEAD) on any path.
#[get("/<_..>")]
pub fn browse() -> Page {
    Page(Status::Ok)
}

/// Every error, both our rejections and Rocket's own (unmatched routes, etc).
#[catch(default)]
pub fn catcher(status: Status, _: &Request<'_>) -> Page {
    Page(status)
}

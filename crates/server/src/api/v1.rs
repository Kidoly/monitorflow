//! The public API (ADR-0101): `/api/v1/<path>` is `/api/<path>`, served
//! by the same router and handlers, with API keys accepted, per-key rate
//! limits and `Idempotency-Key` on writes ([`idempotency`]). Routes for people in a browser
//! are left out, and those that act as the person rather than within a
//! key's reach ([`left_out`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use uuid::Uuid;

use super::{idempotency, openapi, router, ApiError, ApiState};

/// Path of the public API.
pub const PREFIX: &str = "/api/v1";
/// Requests a key may make per window.
pub const REQUESTS_PER_MINUTE: u32 = 600;
const WINDOW: Duration = Duration::from_secs(60);
/// Keys tracked at once; older windows are dropped beyond it.
const MAX_KEYS: usize = 10_000;

/// First segments after `/api/` that v1 does not serve: they are for
/// people in a browser, or would let a key make a credential that
/// outlives it or acts beyond its reach (another key, a display link).
const LEFT_OUT: [&str; 7] = [
    "auth",
    "setup",
    "password-link",
    "acknowledge-link",
    "display",
    "display-links",
    "api-keys",
];

/// Marks a request that came through `/api/v1`.
#[derive(Debug, Clone, Copy)]
struct V1;

/// What the request's key was, and its rate limit, filled in when the key
/// is checked and read back for the answer's headers and the audit log.
#[derive(Debug, Clone, Default)]
pub(super) struct Slot(Arc<Mutex<Used>>);

#[derive(Debug, Default)]
struct Used {
    key: Option<Uuid>,
    rate: Option<Rate>,
}

tokio::task_local! {
    static SLOT: Slot;
}

/// Every API route, with the public API in front of it.
pub fn app(state: ApiState) -> Router {
    let inner = router(state.clone());
    let v1 = inner.clone();
    Router::new()
        .route(&format!("{PREFIX}/openapi.json"), get(openapi::document))
        .route(
            &format!("{PREFIX}/{{*rest}}"),
            any(move |request: Request| handle(state.clone(), v1.clone(), request)),
        )
        .merge(inner)
}

pub(super) fn is_v1(extensions: &Extensions) -> bool {
    extensions.get::<V1>().is_some()
}

/// The key of the request being handled, if it came with one.
pub(super) fn current_key() -> Option<Uuid> {
    SLOT.try_with(|slot| slot.lock().key).ok().flatten()
}

impl Slot {
    fn lock(&self) -> std::sync::MutexGuard<'_, Used> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether v1 leaves out the web interface's route `method` `path`
/// (`/api/…`, a request's path or a route's template). Beyond
/// [`LEFT_OUT`], these act as a person rather than within a key's reach:
/// the display links of a dashboard, which read as their maker
/// (ADR-0099); the person's own on-call contact, where their pages go;
/// and inviting a user or issuing a password link, which hand out a
/// person's credential that would outlive the key.
pub fn left_out(method: &str, path: &str) -> bool {
    let segments: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();
    let post = method.eq_ignore_ascii_case("POST");
    match segments.as_slice() {
        [first, ..] if LEFT_OUT.contains(first) => true,
        ["dashboards", _, "display-links"] | ["on-call", "contact", ..] => true,
        ["users"] | ["users", _, "link"] => post,
        _ => false,
    }
}

/// The path `/api/<rest>` for `/api/v1/<rest>`, unless v1 leaves it out.
fn target(method: &str, path: &str) -> Option<String> {
    let target = format!("/api{}", path.strip_prefix(PREFIX)?);
    (!left_out(method, &target)).then_some(target)
}

async fn handle(state: ApiState, inner: Router, request: Request) -> Response {
    let Some(path) = target(request.method().as_str(), request.uri().path()) else {
        return ApiError::NotFound.into_response();
    };
    let uri = match request.uri().query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let Ok(uri) = uri.parse() else {
        return ApiError::NotFound.into_response();
    };
    // A new request without the outer router's extensions: its path
    // parameters and matched path would be added to the inner router's.
    let (parts, body) = request.into_parts();
    let mut request = Request::new(body);
    *request.method_mut() = parts.method;
    *request.uri_mut() = uri;
    *request.version_mut() = parts.version;
    *request.headers_mut() = parts.headers;
    let slot = Slot::default();
    request.extensions_mut().insert(V1);
    request.extensions_mut().insert(slot.clone());
    let mut response = SLOT
        .scope(slot.clone(), idempotency::run(state, inner, request))
        .await;
    if let Some(rate) = slot.lock().rate {
        rate.headers(&mut response);
    }
    response
}

/// Counts a request of `key` against its rate limit and remembers the key
/// for the audit log; 429 over the limit.
pub(super) fn admit(parts: &Parts, state: &ApiState, key: Uuid) -> Result<(), ApiError> {
    let rate = state.api_key_limits.count(key, Instant::now());
    if let Some(slot) = parts.extensions.get::<Slot>() {
        let mut used = slot.lock();
        used.key = Some(key);
        used.rate = Some(rate);
    }
    if !rate.allowed {
        return Err(ApiError::TooManyRequests(format!(
            "This API key made more than {} requests in a minute. Try again in {} seconds.",
            rate.limit, rate.reset_s
        )));
    }
    Ok(())
}

/// Where a key stands in its current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rate {
    limit: u32,
    remaining: u32,
    /// Seconds until the window ends.
    reset_s: u64,
    allowed: bool,
}

impl Rate {
    fn headers(self, response: &mut Response) {
        let headers = response.headers_mut();
        let mut set = |name: &'static str, value: u64| {
            headers.insert(HeaderName::from_static(name), HeaderValue::from(value));
        };
        set("ratelimit-limit", self.limit.into());
        set("ratelimit-remaining", self.remaining.into());
        set("ratelimit-reset", self.reset_s);
        if !self.allowed {
            set("retry-after", self.reset_s);
        }
    }
}

/// Requests per key in fixed one-minute windows, in memory per instance
/// of the central server.
#[derive(Debug)]
pub struct Limits {
    per_window: u32,
    windows: Mutex<HashMap<Uuid, (u32, Instant)>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self::new(REQUESTS_PER_MINUTE)
    }
}

impl Limits {
    pub fn new(per_window: u32) -> Self {
        Self {
            per_window,
            windows: Mutex::default(),
        }
    }

    fn count(&self, key: Uuid, now: Instant) -> Rate {
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        if windows.len() >= MAX_KEYS && !windows.contains_key(&key) {
            windows.retain(|_, (_, start)| now.duration_since(*start) < WINDOW);
        }
        let (used, start) = windows.entry(key).or_insert((0, now));
        if now.duration_since(*start) >= WINDOW {
            *used = 0;
            *start = now;
        }
        let allowed = *used < self.per_window;
        if allowed {
            *used += 1;
        }
        let left = WINDOW.saturating_sub(now.duration_since(*start));
        Rate {
            limit: self.per_window,
            remaining: self.per_window - *used,
            reset_s: left.as_secs() + u64::from(left.subsec_nanos() > 0),
            allowed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_the_web_interface_s() {
        assert_eq!(
            target("GET", "/api/v1/servers").as_deref(),
            Some("/api/servers")
        );
        assert_eq!(
            target("POST", "/api/v1/servers/x/commands").as_deref(),
            Some("/api/servers/x/commands")
        );
        for (method, left_out) in [
            ("POST", "/api/v1/auth/login"),
            ("GET", "/api/v1/auth/me"),
            ("POST", "/api/v1/setup"),
            ("POST", "/api/v1/password-link/use"),
            ("POST", "/api/v1/acknowledge-link"),
            ("GET", "/api/v1/display"),
            ("POST", "/api/v1/display/data"),
            ("GET", "/api/v1/api-keys"),
            ("DELETE", "/api/v1/api-keys/x"),
            ("POST", "/api/v1/display-links"),
            ("DELETE", "/api/v1/display-links/x"),
            ("GET", "/api/v1/dashboards/x/display-links"),
            ("GET", "/api/v1/on-call/contact"),
            ("PUT", "/api/v1/on-call/contact"),
            ("POST", "/api/v1/on-call/contact/test"),
            ("POST", "/api/v1/users"),
            ("POST", "/api/v1/users/x/link"),
        ] {
            assert_eq!(target(method, left_out), None, "{method} {left_out}");
        }
        assert_eq!(target("GET", "/api/servers"), None);
    }

    #[test]
    fn what_a_key_may_still_do_next_to_what_is_left_out() {
        for (method, path) in [
            ("GET", "/api/v1/users"),
            ("POST", "/api/v1/users/x/roles"),
            ("PUT", "/api/v1/users/x/disabled"),
            ("DELETE", "/api/v1/users/x"),
            ("GET", "/api/v1/dashboards/x"),
            ("POST", "/api/v1/dashboards"),
            ("GET", "/api/v1/on-call/schedules"),
            ("GET", "/api/v1/on-call/people"),
        ] {
            assert!(target(method, path).is_some(), "{method} {path}");
        }
    }

    #[test]
    fn keys_are_limited_per_window() {
        let limits = Limits::new(2);
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let start = Instant::now();
        let first = limits.count(a, start);
        assert_eq!(
            (first.remaining, first.allowed, first.reset_s),
            (1, true, 60)
        );
        assert!(limits.count(a, start + Duration::from_secs(1)).allowed);
        let over = limits.count(a, start + Duration::from_millis(30_500));
        assert_eq!((over.remaining, over.allowed, over.reset_s), (0, false, 30));
        // Another key has its own window.
        assert!(limits.count(b, start).allowed);
        // A new window starts after a minute.
        let next = limits.count(a, start + WINDOW);
        assert_eq!((next.remaining, next.allowed), (1, true));
    }
}

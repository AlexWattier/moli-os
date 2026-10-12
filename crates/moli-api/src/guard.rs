//! DNS-rebinding and cross-site guard for every surface.
//!
//! A web page on any site can make a browser on the LAN send requests to
//! this server; it cannot forge the `Host` header. Only the names this
//! instance is really reached by are accepted, and any address typed as
//! such: DNS rebinding takes a name the attacker controls, never an IP, so a
//! new installation answers on its own address with nothing to configure.
//!
//! Nor can such a page hide where it comes from: a browser says it
//! (`Origin`, `Sec-Fetch-Site`) on every write, and a write from another
//! site is refused. The home network trusts the dashboard, not every page
//! its browsers open. Other clients (agents, scripts, the app's own reports)
//! send neither header and are judged as before.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

#[derive(Clone, Debug)]
pub(crate) struct AllowedHosts(Arc<[String]>);

impl AllowedHosts {
    pub(crate) fn new(extra: &[String]) -> Self {
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .into_iter()
            .map(String::from)
            // Entries go through the same normalization as the header, so
            // "moli:8790" or "[fe80::1]" in the config behave as expected.
            .chain(
                extra
                    .iter()
                    .map(|h| strip_port(h.trim()).to_ascii_lowercase()),
            )
            .collect();
        hosts.dedup();
        Self(hosts.into())
    }

    fn allows(&self, host_header: &str) -> bool {
        let host = strip_port(host_header).to_ascii_lowercase();
        host.parse::<std::net::IpAddr>().is_ok() || self.0.contains(&host)
    }
}

/// `host:port`, `[v6]:port`, `host` → host.
fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !host.contains(':') => {
            host
        }
        _ => authority,
    }
}

/// A write a browser sends from another site's page (a form, a script).
fn cross_site(request: &Request) -> bool {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return false;
    }
    let headers = request.headers();
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if matches!(site, Some("cross-site" | "same-site")) {
        return true;
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // `scheme://authority`, nothing after: the authority must be this very
    // host (`null`, from a sandboxed or local page, is no host).
    origin
        .split_once("://")
        .is_none_or(|(_, authority)| !authority.eq_ignore_ascii_case(host))
}

pub(crate) async fn check(
    State(allowed): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    match host {
        Some(host) if allowed.allows(host) => {
            if cross_site(&request) {
                return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
            }
            next.run(request).await
        }
        _ => (
            StatusCode::FORBIDDEN,
            "host not allowed (add it to server.allowed_hosts)",
        )
            .into_response(),
    }
}

/// What every answer tells browsers: never shown inside another page (an
/// "approve" button cannot be clicked through a disguise), content of the
/// type declared, no address of the house in what is sent to other sites,
/// and the house's data never kept in a cache. A route that says otherwise
/// (an image's own policy, its cache) keeps its word.
pub(crate) async fn headers(request: Request, next: Next) -> Response {
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static("frame-ancestors 'none'"));
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    if api {
        headers
            .entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        let allowed = AllowedHosts::new(&["192.168.0.62".into(), "Moli".into()]);
        assert!(allowed.allows("localhost:8790"));
        assert!(allowed.allows("127.0.0.1"));
        assert!(allowed.allows("[::1]:8790"));
        assert!(allowed.allows("192.168.0.62:8790"));
        assert!(allowed.allows("moli:8790"));
        assert!(!allowed.allows("evil.example:8790"));
        assert!(!allowed.allows("192.168.0.62.evil.example"));
        // Any address typed as such: rebinding needs a name.
        assert!(allowed.allows("10.0.0.5:8790"));
        assert!(allowed.allows("[fe80::2]:8790"));
        assert!(!allowed.allows("moli.evil.example:8790"));

        let with_ports = AllowedHosts::new(&["moli:8790".into(), "[fe80::1]".into()]);
        assert!(with_ports.allows("moli:8790"));
        assert!(with_ports.allows("[fe80::1]:8790"));
    }

    fn request(method: Method, headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri("/api/command")
            .header(header::HOST, "192.168.0.62:8790");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn writes_from_another_site_are_refused() {
        // The dashboard itself, and clients that are not browsers.
        let own = [
            ("origin", "http://192.168.0.62:8790"),
            ("sec-fetch-site", "same-origin"),
        ];
        assert!(!cross_site(&request(Method::POST, &own)));
        assert!(!cross_site(&request(Method::POST, &[])));
        // A page elsewhere, by its origin or by what the browser says.
        assert!(cross_site(&request(
            Method::POST,
            &[("origin", "https://evil.example")]
        )));
        assert!(cross_site(&request(Method::PUT, &[("origin", "null")])));
        assert!(cross_site(&request(
            Method::POST,
            &[("sec-fetch-site", "cross-site")]
        )));
        assert!(cross_site(&request(
            Method::DELETE,
            &[("origin", "http://192.168.0.62:9999")]
        )));
        // Reading is never a write.
        assert!(!cross_site(&request(
            Method::GET,
            &[("origin", "https://evil.example")]
        )));
    }
}

//! Validated dashboard perimeter shared by HTTP, CORS, and WebSocket upgrades.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{header, uri::Authority, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Invalid dashboard startup configuration. No DNS resolution is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityError {
    /// The bind host is not a literal loopback IP address.
    InvalidBind,
    /// Port zero cannot provide a stable browser origin.
    InvalidPort,
    /// An allowed origin is not an HTTP(S) scheme/host/port tuple.
    InvalidOrigin,
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidBind => "dashboard bind host must be a literal loopback IP address",
            Self::InvalidPort => "dashboard ports must be between 1 and 65535",
            Self::InvalidOrigin => "allowed origins must be HTTP(S) origins without credentials, paths, queries, fragments, or wildcards",
        })
    }
}

impl std::error::Error for SecurityError {}

/// Parses a literal loopback address for CLI configuration; never resolves DNS.
pub fn parse_loopback_ip(host: &str) -> Result<IpAddr, SecurityError> {
    let ip: IpAddr = host.parse().map_err(|_| SecurityError::InvalidBind)?;
    if !ip.is_loopback() {
        return Err(SecurityError::InvalidBind);
    }
    Ok(ip)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: &'static str,
    host: String,
    port: u16,
}

impl Origin {
    fn parse(value: &str) -> Result<Self, SecurityError> {
        let (scheme, authority) = value
            .split_once("://")
            .ok_or(SecurityError::InvalidOrigin)?;
        let (scheme, default_port) = match scheme {
            "http" => ("http", 80),
            "https" => ("https", 443),
            _ => return Err(SecurityError::InvalidOrigin),
        };
        // Authority parsing alone is not URL/origin validation: explicitly exclude
        // userinfo, URL suffixes, encoded hosts, and browser backslash ambiguity.
        if authority.is_empty()
            || !authority.is_ascii()
            || authority.bytes().any(|b| {
                b.is_ascii_whitespace()
                    || b.is_ascii_control()
                    || matches!(b, b'/' | b'\\' | b'?' | b'#' | b'@' | b'%')
            })
        {
            return Err(SecurityError::InvalidOrigin);
        }
        let authority: Authority = authority
            .parse()
            .map_err(|_| SecurityError::InvalidOrigin)?;
        let raw_host = authority.host();
        let host = if let Some(ipv6) = raw_host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
        {
            let ip: Ipv6Addr = ipv6.parse().map_err(|_| SecurityError::InvalidOrigin)?;
            format!("[{ip}]")
        } else if let Ok(ip) = raw_host.parse::<Ipv4Addr>() {
            ip.to_string()
        } else {
            // Restrict DNS names to ASCII labels (use punycode for IDNs). Numeric
            // aliases such as 127.1 are excluded rather than browser-normalized.
            if raw_host.len() > 253
                || raw_host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                || !raw_host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label.starts_with(|c: char| c.is_ascii_alphanumeric())
                        && label.ends_with(|c: char| c.is_ascii_alphanumeric())
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return Err(SecurityError::InvalidOrigin);
            }
            raw_host.to_ascii_lowercase()
        };
        // Authority::port() returns None for an invalid explicit port too.
        // Inspect the suffix so malformed ports never become scheme defaults.
        let suffix = authority
            .as_str()
            .strip_prefix(raw_host)
            .ok_or(SecurityError::InvalidOrigin)?;
        let port = if suffix.is_empty() {
            default_port
        } else {
            let digits = suffix
                .strip_prefix(':')
                .ok_or(SecurityError::InvalidOrigin)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(SecurityError::InvalidOrigin);
            }
            digits
                .parse::<u16>()
                .map_err(|_| SecurityError::InvalidOrigin)?
        };
        if port == 0 {
            return Err(SecurityError::InvalidOrigin);
        }
        Ok(Self { scheme, host, port })
    }
}

/// An immutable allowlist; trust is never inferred from request/proxy headers.
#[derive(Debug, Clone)]
pub struct DashboardSecurity {
    origins: Vec<Origin>,
}

impl Default for DashboardSecurity {
    fn default() -> Self {
        Self::local_origins(3000)
    }
}

impl DashboardSecurity {
    fn local_origins(port: u16) -> Self {
        Self {
            origins: ["localhost", "127.0.0.1", "[::1]"]
                .into_iter()
                .map(|host| Origin {
                    scheme: "http",
                    host: host.into(),
                    port,
                })
                .collect(),
        }
    }

    /// Validates a loopback listener and adds explicit browser origins to its
    /// localhost defaults. Invalid configuration fails before server startup.
    pub fn new(bind: SocketAddr, additional_origins: &[String]) -> Result<Self, SecurityError> {
        if !bind.ip().is_loopback() {
            return Err(SecurityError::InvalidBind);
        }
        if bind.port() == 0 {
            return Err(SecurityError::InvalidPort);
        }
        let mut policy = Self::local_origins(bind.port());
        // Permit a selected 127/8 loopback literal without trusting incoming Host.
        let bound_origin = Origin::parse(&format!("http://{bind}"))?;
        policy.origins.push(bound_origin);
        for value in additional_origins {
            let origin = Origin::parse(value)?;
            if !policy.origins.contains(&origin) {
                policy.origins.push(origin);
            }
        }
        Ok(policy)
    }

    pub(crate) fn allows_origin(&self, value: &str) -> bool {
        Origin::parse(value).is_ok_and(|origin| self.origins.contains(&origin))
    }

    fn allows_host(&self, authority: &str) -> bool {
        self.origins.iter().any(|allowed| {
            Origin::parse(&format!("{}://{authority}", allowed.scheme))
                .is_ok_and(|origin| origin.host == allowed.host && origin.port == allowed.port)
        })
    }
}

/// Returns one unambiguous UTF-8 header, rejecting repeated values.
fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

/// Protect all routes, including origin-less same-origin REST fetches against
/// DNS rebinding. This runs before CORS and the WebSocket upgrade extractor.
pub(crate) async fn enforce_perimeter(
    State(policy): State<DashboardSecurity>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let authority = request.uri().authority().map(Authority::as_str);
    let host = if headers.contains_key(header::HOST) {
        single_header(headers, header::HOST)
    } else {
        authority // HTTP/2 may supply :authority instead of Host.
    };
    let host_allowed = host.is_some_and(|host| policy.allows_host(host))
        && authority.map_or(true, |authority| policy.allows_host(authority));
    let origin_allowed = if headers.contains_key(header::ORIGIN) {
        single_header(headers, header::ORIGIN).is_some_and(|origin| policy.allows_origin(origin))
    } else {
        request.uri().path() != "/ws/telemetry"
    };
    if !host_allowed || !origin_allowed {
        return (StatusCode::FORBIDDEN, "Untrusted dashboard host or origin").into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_loopback_listener_and_port() {
        for address in ["0.0.0.0:3000", "[::]:3000", "192.0.2.1:3000", "127.0.0.1:0"] {
            assert!(DashboardSecurity::new(address.parse().unwrap(), &[]).is_err());
        }
        for address in ["127.0.0.1:3001", "[::1]:3001", "127.0.0.2:3001"] {
            let policy = DashboardSecurity::new(address.parse().unwrap(), &[]).unwrap();
            assert!(policy.allows_origin(&format!("http://{address}")));
            assert!(!policy.allows_origin("http://localhost:3000"));
        }
    }

    #[test]
    fn rejects_invalid_configured_origins() {
        for value in [
            "",
            "null",
            "*",
            "https://*.example.com",
            "ws://example.com",
            "ftp://example.com",
            "https://example.com/",
            "https://example.com/path",
            "https://user:password@example.com",
            "https://example.com?query",
            "https://example.com#fragment",
            " https://example.com",
            "https://example.com ",
            "https://example.com:0",
            "https://example.com:65536",
            "https://example.com:",
            "https://example.com:abc",
            "https://example.com\\evil",
            "https://example%2ecom",
            "https://-bad.example",
            "https://bad_.example",
            "https://example.com.",
            "https://127.1",
            "https://[::1%25eth0]",
            "https://[invalid]",
        ] {
            assert!(
                DashboardSecurity::new("127.0.0.1:3000".parse().unwrap(), &[value.into()]).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn matches_scheme_host_and_effective_port_only() {
        let policy = DashboardSecurity::new(
            "127.0.0.1:3000".parse().unwrap(),
            &[
                "https://Dashboard.Example:443".into(),
                "https://[::1]:8443".into(),
            ],
        )
        .unwrap();
        for origin in [
            "https://dashboard.example",
            "https://dashboard.example:443",
            "https://[::1]:8443",
        ] {
            assert!(policy.allows_origin(origin), "{origin}");
        }
        for origin in [
            "http://dashboard.example",
            "https://dashboard.example:8443",
            "https://sub.dashboard.example",
            "https://[::1]",
        ] {
            assert!(!policy.allows_origin(origin), "{origin}");
        }
        assert!(policy.allows_host("dashboard.example"));
        assert!(policy.allows_host("dashboard.example:443"));
        assert!(!policy.allows_host("dashboard.example:3000"));
        assert!(!policy.allows_host("attacker.example"));
    }
}

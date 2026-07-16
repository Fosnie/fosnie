// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Security policy for OAuth 2.1 discovery against a remote MCP server.
//!
//! A remote MCP server controls its own `WWW-Authenticate` header, which names the
//! protected-resource-metadata URL, which names the authorisation servers, which name
//! the authorize / token / registration endpoints. Left unchecked, a hostile or
//! compromised server could point discovery at an internal host and we would post a
//! client secret to it (server-side request forgery). The MCP SDK does not validate any
//! of this chain, so the checks live here.
//!
//! The trust anchor is **an admin approving the issuer**, not a runtime probe. Discovery
//! is an admin-only, interactive operation; the admin approves an issuer by saving its
//! validated metadata, and the connect path thereafter uses only that saved metadata and
//! never re-discovers. This module enforces the mechanical guards that back that model:
//!
//! - cloud-metadata and link-local addresses are refused unconditionally (no override);
//! - discovered endpoints must be `https`;
//! - a discovered authorisation server must be same-origin with the MCP server, or an
//!   origin the admin has explicitly declared (so on-prem RFC 1918 issuers work — but
//!   only because a human wrote the origin down, never because a server claimed it);
//! - every discovered host is resolved and its addresses re-checked (DNS-rebinding).
//!
//! Plus three protocol guards the SDK omits: PKCE S256 must be advertised; the
//! authorisation-response `iss` (where the server supports it) must match the approved
//! issuer by exact string comparison; and the RFC 8707 `resource` value is normalised.
//!
//! This module deliberately takes plain strings, not SDK metadata types, so it stays
//! self-contained, unit-testable without a network, and cheap to retire if the SDK grows
//! an equivalent guard.

use std::net::{IpAddr, ToSocketAddrs};

use crate::error::{AppError, Result};
use crate::mcp::validate::is_link_local;

/// Host names that resolve to a cloud instance-metadata service. Refused whatever the
/// admin declares: no legitimate authorisation server lives behind one, and reaching it
/// with a bearer/secret attached is the classic confused-deputy credential theft.
const CLOUD_METADATA_HOSTS: &[&str] = &["metadata", "metadata.google.internal", "metadata.goog"];

/// True iff `host` names a cloud instance-metadata service (case-insensitive).
fn is_cloud_metadata_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    CLOUD_METADATA_HOSTS.contains(&host.as_str())
}

/// True iff `ip` is a cloud instance-metadata address that is NOT already caught by the
/// link-local check. Notably AWS' IPv6 metadata endpoint `fd00:ec2::254` sits in the
/// unique-local `fc00::/7` block, so it would otherwise read as an ordinary (admin-
/// declarable) private address. Block the whole `fd00:ec2::/32` reservation.
fn is_cloud_metadata_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets() == [169, 254, 169, 254],
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd00 && s[1] == 0x0ec2
        }
    }
}

/// An address that must never be reached during OAuth discovery or token exchange,
/// regardless of admin declaration: link-local (incl. 169.254.169.254) and cloud
/// metadata. Private/internal addresses are deliberately NOT here — those are reachable
/// when, and only when, an admin has declared the issuer origin.
fn is_forbidden_ip(ip: &IpAddr) -> bool {
    is_link_local(ip) || is_cloud_metadata_ip(ip)
}

/// The normalised origin (`scheme://host[:port]`, default port applied) of a parsed URL.
fn origin_of(u: &reqwest::Url) -> String {
    let scheme = u.scheme();
    let host = u.host_str().unwrap_or_default();
    match u.port_or_known_default() {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    }
}

/// The normalised origin of a URL string, or `None` if it does not parse / has no host.
pub fn origin_str(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    u.host_str()?;
    Some(origin_of(&u))
}

/// Validate a single endpoint URL discovered from a remote MCP server before we send it
/// anything. Enforces, in order: https; not a cloud-metadata host; same-origin with the
/// MCP server or an admin-declared origin; and resolves only to non-forbidden addresses
/// (DNS-rebinding re-check). A cross-origin authorisation server the admin did not name
/// is refused with a message naming the origin, so the admin can declare it and retry.
///
/// `server_url` is the MCP server's own URL; `allowed_issuer_origin` is an origin the
/// admin explicitly entered on the discover form (`scheme://host[:port]`), or `None`.
pub async fn validate_discovered_endpoint(
    url: &str,
    server_url: &str,
    allowed_issuer_origin: Option<&str>,
) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| AppError::Validation(format!("invalid discovered OAuth endpoint URL: {url}")))?;

    if parsed.scheme() != "https" {
        return Err(AppError::Validation(format!(
            "discovered OAuth endpoint must use https, got '{}': {url}",
            parsed.scheme()
        )));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::Validation(format!("discovered OAuth endpoint has no host: {url}")))?
        .to_string();
    if is_cloud_metadata_host(&host) {
        return Err(AppError::Validation(format!(
            "discovered OAuth endpoint host '{host}' is a cloud instance-metadata service; refused"
        )));
    }

    // Same-origin-or-declared. This is what makes admin approval the trust anchor: a
    // cross-origin authorisation server is reachable only if the admin named its origin.
    let disc_origin = origin_of(&parsed);
    let srv_origin = origin_str(server_url).ok_or_else(|| {
        AppError::Validation(format!("MCP server URL has no valid origin: {server_url}"))
    })?;
    let declared_ok = allowed_issuer_origin
        .and_then(origin_str)
        .map(|declared| declared == disc_origin)
        .unwrap_or(false);
    if disc_origin != srv_origin && !declared_ok {
        return Err(AppError::Validation(format!(
            "discovered authorisation server origin {disc_origin} does not match the MCP server \
             origin {srv_origin} and was not declared; set it as the allowed issuer origin to \
             approve it deliberately"
        )));
    }

    // Resolve and re-check every address (DNS rebinding). The resolver is blocking, so
    // run it off the async worker; the OAuth path is hotter than register/approve.
    let port = parsed.port_or_known_default().unwrap_or(443);
    let resolve_host = host.clone();
    let addrs: Vec<IpAddr> = tokio::task::spawn_blocking(move || {
        (resolve_host.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.map(|sa| sa.ip()).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("OAuth endpoint resolver task failed: {e}")))?
    .map_err(|e| {
        AppError::Validation(format!("discovered OAuth endpoint host '{host}' does not resolve: {e}"))
    })?;
    if addrs.is_empty() {
        return Err(AppError::Validation(format!(
            "discovered OAuth endpoint host '{host}' resolved to no addresses"
        )));
    }
    for ip in &addrs {
        if is_forbidden_ip(ip) {
            return Err(AppError::Validation(format!(
                "discovered OAuth endpoint {url} resolves to {ip}, a link-local/cloud-metadata \
                 address; refused"
            )));
        }
    }
    Ok(())
}

/// Require that the authorisation server advertises the S256 PKCE method. OAuth 2.1
/// mandates it; a server that omits `code_challenge_methods_supported` entirely is the
/// exact non-compliant case the requirement targets, so absence is a refusal, not a
/// silent pass. `methods` is the server's `code_challenge_methods_supported`, if any.
pub fn enforce_pkce_s256(methods: Option<&[String]>) -> Result<()> {
    match methods {
        None => Err(AppError::Validation(
            "authorisation server does not advertise code_challenge_methods_supported; PKCE S256 \
             is required, refusing"
                .into(),
        )),
        Some(m) if m.iter().any(|x| x == "S256") => Ok(()),
        Some(_) => Err(AppError::Validation(
            "authorisation server advertises PKCE methods but not S256; refusing".into(),
        )),
    }
}

/// Validate the authorisation-response `iss` parameter against the approved issuer.
/// Where the server supports the parameter, it MUST be present and MUST equal the
/// approved issuer by **exact string comparison** — no case-folding, no port or
/// trailing-slash normalisation, no percent-decoding (any of those would let a lookalike
/// issuer slip through). Where the server does not support it, there is nothing to check.
pub fn validate_callback_iss(
    iss_param_supported: bool,
    callback_iss: Option<&str>,
    approved_issuer: &str,
) -> Result<()> {
    if !iss_param_supported {
        return Ok(());
    }
    match callback_iss {
        None => Err(AppError::Validation(
            "authorisation response is missing the required iss parameter; refusing".into(),
        )),
        Some(got) if got == approved_issuer => Ok(()),
        Some(got) => Err(AppError::Validation(format!(
            "authorisation response iss '{got}' does not match the approved issuer \
             '{approved_issuer}'; refusing"
        ))),
    }
}

/// Normalise a URL for use as an RFC 8707 `resource` value: drop a lone trailing slash on
/// an otherwise-empty path, which some authorisation servers match strictly against the
/// token audience. A URL that carries a real path is returned unchanged.
pub fn normalise_resource_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) if u.path() == "/" && u.query().is_none() && u.fragment().is_none() => {
            let s = u.as_str();
            s.strip_suffix('/').unwrap_or(s).to_string()
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_metadata_host_detected() {
        assert!(is_cloud_metadata_host("metadata.google.internal"));
        assert!(is_cloud_metadata_host("METADATA.GOOGLE.INTERNAL"));
        assert!(is_cloud_metadata_host("metadata.goog"));
        assert!(is_cloud_metadata_host("metadata.google.internal.")); // trailing dot
        assert!(!is_cloud_metadata_host("login.microsoftonline.com"));
        assert!(!is_cloud_metadata_host("github.com"));
    }

    #[test]
    fn cloud_metadata_ip_detected() {
        assert!(is_cloud_metadata_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_cloud_metadata_ip(&"fd00:ec2::254".parse().unwrap())); // AWS IMDS v6, a ULA
        assert!(!is_cloud_metadata_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_cloud_metadata_ip(&"fd12:3456::1".parse().unwrap())); // other ULA is fine
        assert!(is_forbidden_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_forbidden_ip(&"fe80::1".parse().unwrap())); // link-local
        assert!(is_forbidden_ip(&"fd00:ec2::254".parse().unwrap()));
        assert!(!is_forbidden_ip(&"10.0.0.1".parse().unwrap())); // private, admin-declarable
        assert!(!is_forbidden_ip(&"140.82.112.3".parse().unwrap())); // public
    }

    #[tokio::test]
    async fn rejects_http_scheme_unconditionally() {
        let err = validate_discovered_endpoint(
            "http://login.example.com/authorize",
            "https://mcp.example.com/mcp",
            Some("http://login.example.com"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("https"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_cloud_metadata_host_even_when_declared() {
        // The admin cannot declare their way to the metadata service.
        let err = validate_discovered_endpoint(
            "https://metadata.google.internal/token",
            "https://mcp.example.com/mcp",
            Some("https://metadata.google.internal"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("metadata"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_undeclared_cross_origin_issuer() {
        // Refused at the origin gate before any resolution — IP literals keep it offline.
        let err = validate_discovered_endpoint(
            "https://10.9.9.9/authorize",
            "https://10.1.1.1/mcp",
            None,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not match") && msg.contains("10.9.9.9"), "got: {msg}");
    }

    #[tokio::test]
    async fn accepts_declared_private_ip_issuer() {
        // The on-prem case: an authorisation server on RFC 1918 space that the admin
        // explicitly declared. It MUST pass — anyone later "hardening" the gate to blanket-
        // reject private addresses would break every on-prem deployment. IP literals mean
        // the resolver step touches no DNS.
        let r = validate_discovered_endpoint(
            "https://10.9.9.9/authorize",
            "https://10.1.1.1/mcp",
            Some("https://10.9.9.9"),
        )
        .await;
        assert!(r.is_ok(), "declared private-IP issuer should pass: {r:?}");
    }

    #[tokio::test]
    async fn accepts_same_origin_private_ip_issuer() {
        let r = validate_discovered_endpoint(
            "https://10.1.1.1/authorize",
            "https://10.1.1.1/mcp",
            None,
        )
        .await;
        assert!(r.is_ok(), "same-origin private-IP issuer should pass: {r:?}");
    }

    #[tokio::test]
    async fn rejects_declared_metadata_ip() {
        // Declaring a link-local/metadata origin does not make it reachable.
        let err = validate_discovered_endpoint(
            "https://169.254.169.254/authorize",
            "https://10.1.1.1/mcp",
            Some("https://169.254.169.254"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("link-local/cloud-metadata"), "got: {err}");
    }

    #[test]
    fn s256_absent_is_refused() {
        // The SDK is silent on this exact case; we must refuse.
        assert!(enforce_pkce_s256(None).is_err());
    }

    #[test]
    fn s256_present_without_s256_is_refused() {
        let plain = vec!["plain".to_string()];
        assert!(enforce_pkce_s256(Some(&plain)).is_err());
    }

    #[test]
    fn s256_present_is_accepted() {
        let ok = vec!["plain".to_string(), "S256".to_string()];
        assert!(enforce_pkce_s256(Some(&ok)).is_ok());
    }

    #[test]
    fn iss_exact_match_only() {
        let issuer = "https://login.example.com";
        assert!(validate_callback_iss(true, Some(issuer), issuer).is_ok());
        // Missing when required → refuse.
        assert!(validate_callback_iss(true, None, issuer).is_err());
        // Case-differing → refuse (no case-folding).
        assert!(validate_callback_iss(true, Some("https://LOGIN.example.com"), issuer).is_err());
        // Trailing-slash-differing → refuse (no normalisation).
        assert!(validate_callback_iss(true, Some("https://login.example.com/"), issuer).is_err());
        // Not supported by the server → nothing to check.
        assert!(validate_callback_iss(false, None, issuer).is_ok());
    }

    #[test]
    fn resource_drops_lone_trailing_slash() {
        assert_eq!(normalise_resource_url("https://mcp.example.com/"), "https://mcp.example.com");
        // A real path is preserved verbatim.
        assert_eq!(normalise_resource_url("https://mcp.example.com/mcp"), "https://mcp.example.com/mcp");
        // A trailing slash on a real path is NOT stripped (it may be significant).
        assert_eq!(normalise_resource_url("https://mcp.example.com/mcp/"), "https://mcp.example.com/mcp/");
    }
}

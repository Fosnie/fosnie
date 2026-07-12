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

//! Endpoint validation for streamable-HTTP MCP servers (FEATURE B1, acceptance #2/#6).
//!
//! Two modes, both an SSRF guard:
//! - **Private-only** (`allow_remote = false`, the default): a client-internal server.
//!   Accept only loopback + RFC1918 / IPv6-ULA; reject anything globally routable.
//! - **Remote** (`allow_remote = true`, admin explicitly opted a server into egress):
//!   a public server (GitHub / Cloudflare / Context7). Require `https`, and still
//!   reject the 169.254.169.254 cloud-metadata endpoint + link-local ranges
//!   (confused-deputy / SSRF defence). Sovereignty = admin gate + egress opt-in + audit,
//!   not "nothing leaves".
//!
//! Both modes reject non-http(s) schemes and credentials embedded in the URL. Resolution
//! is done at registration AND re-checked at connect (DNS-rebinding TOCTOU).

use std::net::{IpAddr, ToSocketAddrs};

use crate::error::{AppError, Result};

/// True iff `ip` is a private/internal address an MCP server may legitimately use.
/// Loopback + RFC1918 (v4) and loopback + unique-local fc00::/7 (v6, incl. v4-mapped).
pub fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            if (v6.octets()[0] & 0xfe) == 0xfc {
                return true; // unique-local fc00::/7
            }
            // IPv4-mapped (::ffff:a.b.c.d) — judge the embedded v4.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.is_loopback() || v4.is_private();
            }
            false
        }
    }
}

/// True iff `ip` is link-local — the 169.254.0.0/16 range (incl. the 169.254.169.254
/// cloud-metadata endpoint) or IPv6 fe80::/10. Refused in BOTH modes: a remote server
/// must never resolve to the metadata service (confused-deputy / credential theft).
fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return true; // fe80::/10
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.is_link_local();
            }
            false
        }
    }
}

/// Validate a streamable-HTTP MCP endpoint. Rejects non-http(s) and embedded
/// credentials in both modes. When `allow_remote` is false, every resolved address
/// must be private/internal. When true, a public server is permitted but `https` is
/// mandatory and link-local / cloud-metadata addresses are still refused.
pub fn validate_endpoint(url: &str, allow_remote: bool) -> Result<()> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| AppError::Validation("invalid MCP server URL".into()))?;
    match parsed.scheme() {
        "https" => {}
        "http" if !allow_remote => {}
        "http" => {
            return Err(AppError::Validation(
                "a remote (egress) MCP server must use https".into(),
            ))
        }
        s => return Err(AppError::Validation(format!("MCP URL scheme must be http(s), got '{s}'"))),
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::Validation("MCP URL must not embed credentials".into()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::Validation("MCP URL has no host".into()))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| AppError::Validation(format!("MCP host does not resolve: {e}")))?
        .map(|sa| sa.ip())
        .collect();
    if addrs.is_empty() {
        return Err(AppError::Validation("MCP host resolved to no addresses".into()));
    }
    for ip in &addrs {
        // Cloud-metadata / link-local is refused regardless of mode.
        if is_link_local(ip) {
            return Err(AppError::Validation(format!(
                "MCP endpoint resolves to {ip}, a link-local/cloud-metadata address (refused)"
            )));
        }
        if !allow_remote && !is_private(ip) {
            return Err(AppError::Validation(format!(
                "MCP endpoint resolves to {ip}, which is not a private/internal address \
                 (mark the server as remote/requires-egress to reach a public endpoint)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_only_rejects_public_and_metadata() {
        assert!(is_private(&"127.0.0.1".parse().unwrap()));
        assert!(is_private(&"10.1.2.3".parse().unwrap()));
        assert!(is_private(&"192.168.0.5".parse().unwrap()));
        assert!(is_private(&"172.16.9.9".parse().unwrap()));
        assert!(!is_private(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private(&"169.254.169.254".parse().unwrap())); // cloud metadata
        assert!(!is_private(&"1.1.1.1".parse().unwrap()));
        // allow_remote = false: private-only behaviour (unchanged).
        assert!(validate_endpoint("https://example.com/mcp", false).is_err());
        assert!(validate_endpoint("ftp://10.0.0.1/mcp", false).is_err());
        assert!(validate_endpoint("http://user:pw@10.0.0.1/mcp", false).is_err());
        assert!(validate_endpoint("http://127.0.0.1:8931/mcp", false).is_ok());
    }

    #[test]
    fn link_local_detected() {
        assert!(is_link_local(&"169.254.169.254".parse().unwrap())); // cloud metadata
        assert!(is_link_local(&"169.254.0.1".parse().unwrap()));
        assert!(is_link_local(&"fe80::1".parse().unwrap()));
        assert!(!is_link_local(&"8.8.8.8".parse().unwrap()));
        assert!(!is_link_local(&"10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn remote_allows_public_https_but_guards_ssrf() {
        // allow_remote = true: a public HTTPS server is permitted…
        assert!(validate_endpoint("https://example.com/mcp", true).is_ok());
        // …but http is refused for a remote server,
        assert!(validate_endpoint("http://example.com/mcp", true).is_err());
        // embedded credentials are still refused,
        assert!(validate_endpoint("https://user:pw@example.com/mcp", true).is_err());
        // and link-local / cloud-metadata is refused even in remote mode.
        assert!(validate_endpoint("https://169.254.169.254/mcp", true).is_err());
    }
}

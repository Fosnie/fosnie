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

//! Talking to an instance over ordinary HTTP: checking one is there, redeeming a
//! pairing code, and the few small calls the shell makes on its own behalf.
//!
//! Everything the *application* fetches goes straight from the web view to the
//! instance, which is the whole reason the web view exists. What is here is only
//! what the shell needs before, or independently of, the application: whether an
//! address is an instance at all, the pairing exchange, whether this device is
//! still trusted, and the name of a chat it wants to raise a notification about.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// A short timeout throughout: every call here is either something a person is
/// waiting on behind a form, or a background check that is better skipped than
/// hung.
const TIMEOUT_SECS: u64 = 15;

pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .user_agent(concat!("fosnie-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not start the HTTP client")
}

/// Normalise what a person typed into a base URL: add a scheme when they gave a
/// bare host, drop trailing slashes so paths concatenate cleanly.
pub fn normalise_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// What was found at an address, for the pairing screen to show before anyone
/// types a code.
#[derive(Debug, serde::Serialize)]
pub struct InstanceInfo {
    pub base_url: String,
    /// How the instance signs its own users in. Not used to authenticate this
    /// client, which presents a device token, but it confirms the platform's own
    /// endpoint answered rather than some other server on that address.
    pub auth_mode: String,
}

#[derive(Deserialize)]
struct AuthConfig {
    mode: String,
}

/// Check that an address is a reachable Fosnie instance, with a message honest
/// enough to act on when it is not.
pub async fn validate(http: &reqwest::Client, raw_url: &str) -> Result<InstanceInfo> {
    let base_url = normalise_base(raw_url);
    let health = http.get(format!("{base_url}/health")).send().await.map_err(|e| {
        if e.is_timeout() {
            anyhow!("{base_url} did not answer in time. Check the address and the network.")
        } else if e.is_connect() {
            anyhow!("Could not reach {base_url}. Check the address, the network, and any VPN.")
        } else {
            anyhow!("Could not reach {base_url}: {e}")
        }
    })?;
    if !health.status().is_success() {
        bail!("{base_url} answered {} to a health check, so it is not ready.", health.status());
    }

    let cfg = http.get(format!("{base_url}/api/auth/config")).send().await;
    let Ok(cfg) = cfg else {
        bail!("{base_url} is reachable but did not answer as a Fosnie instance.");
    };
    if !cfg.status().is_success() {
        bail!(
            "{base_url} is reachable but did not answer as a Fosnie instance ({}). It may be an \
             older release that cannot pair a device.",
            cfg.status()
        );
    }
    let cfg: AuthConfig = cfg
        .json()
        .await
        .map_err(|_| anyhow!("{base_url} is reachable but did not answer as a Fosnie instance."))?;

    Ok(InstanceInfo { base_url, auth_mode: cfg.mode })
}

#[derive(Deserialize)]
struct PairedOut {
    device_id: String,
    token: String,
}

/// Redeem a pairing code for a device token. The code is single-use and the whole
/// authority: this client has no credential of its own until the exchange lands.
pub async fn pair(
    http: &reqwest::Client,
    base_url: &str,
    code: &str,
    name: &str,
    platform: &str,
) -> Result<(String, String)> {
    let res = http
        .post(format!("{base_url}/api/device/pair"))
        .json(&serde_json::json!({ "code": code, "name": name, "platform": platform }))
        .send()
        .await
        .map_err(|e| anyhow!("Could not reach {base_url}: {e}"))?;

    match res.status().as_u16() {
        200 | 201 => {
            let out: PairedOut = res.json().await.context("the instance sent an unusable reply")?;
            Ok((out.device_id, out.token))
        }
        404 => bail!("That code is not valid any more. Codes last ten minutes and work once."),
        429 => bail!("Too many attempts. Wait a few minutes and try again."),
        status => {
            let body = res.text().await.unwrap_or_default();
            bail!("Pairing failed ({status}). {}", body.chars().take(200).collect::<String>())
        }
    }
}

#[derive(Deserialize)]
struct WorkspaceOut {
    id: String,
    path: String,
    tier: String,
}

/// Tell the instance about a folder the person has just connected on this
/// machine, and get back the id that requests will name it by.
///
/// The instance is told the path and the level of trust so it can show the owner
/// what they granted and record what was done in it. What is *in* the folder
/// never leaves this machine.
pub async fn connect_folder(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    path: &str,
    tier: &str,
) -> Result<(String, String, String)> {
    let res = http
        .post(format!("{base_url}/api/me/workspaces"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "path": path, "label": "", "tier": tier }))
        .send()
        .await
        .map_err(|e| anyhow!("Could not reach {base_url}: {e}"))?;
    match res.status().as_u16() {
        200 | 201 => {
            let out: WorkspaceOut =
                res.json().await.context("the instance sent an unusable reply")?;
            Ok((out.id, out.path, out.tier))
        }
        400 => {
            let body = res.text().await.unwrap_or_default();
            bail!("{}", body.chars().take(200).collect::<String>())
        }
        403 => bail!(
            "This instance did not accept the folder. Sign this computer out and pair it again."
        ),
        status => {
            let body = res.text().await.unwrap_or_default();
            bail!("The folder could not be connected ({status}). {}", body.chars().take(200).collect::<String>())
        }
    }
}

/// The folders the instance still holds for this account, so a folder withdrawn
/// from the web stops being one this machine will work in. `None` when the
/// instance could not be asked, which says nothing either way and changes
/// nothing locally.
pub async fn live_workspaces(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Row {
        id: String,
        revoked_at: Option<serde_json::Value>,
    }
    let res = http
        .get(format!("{base_url}/api/me/workspaces"))
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let rows: Vec<Row> = res.json().await.ok()?;
    Some(rows.into_iter().filter(|r| r.revoked_at.is_none()).map(|r| r.id).collect())
}

/// Outcome of the periodic check that this device is still trusted.
#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    /// The token was accepted.
    Valid,
    /// The token was refused: the device has been signed out from the web, or the
    /// account is gone. The pairing is finished and has to be cleared.
    Revoked,
    /// Nothing could be determined — offline, instance down. Says nothing about
    /// the pairing, so nothing is done about it.
    Unknown,
}

/// Ask the instance whether this device's token is still good.
///
/// A revoked device would otherwise sit on a live socket, looking connected,
/// until something happened to make it reconnect. This is what makes signing a
/// machine out from the web take effect on that machine.
pub async fn check_trust(http: &reqwest::Client, base_url: &str, token: &str) -> Trust {
    match http.get(format!("{base_url}/api/whoami")).bearer_auth(token).send().await {
        Ok(res) if res.status() == reqwest::StatusCode::UNAUTHORIZED => Trust::Revoked,
        Ok(res) if res.status().is_success() => Trust::Valid,
        _ => Trust::Unknown,
    }
}

/// Withdraw this device from the account it is paired with. Best effort: signing
/// out locally must not depend on the instance being reachable.
pub async fn revoke_self(http: &reqwest::Client, base_url: &str, token: &str, device_id: &str) {
    let _ = http
        .delete(format!("{base_url}/api/me/devices/{device_id}"))
        .bearer_auth(token)
        .send()
        .await;
}

#[derive(Deserialize)]
struct TicketOut {
    ticket: String,
}

/// Mint a single-use ticket for the socket, so the token never appears in a URL.
/// `Ok(None)` means the token was refused — the caller treats that as revocation.
pub async fn ws_ticket(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<Option<String>> {
    let res = http
        .post(format!("{base_url}/api/ws-ticket"))
        .bearer_auth(token)
        .send()
        .await
        .context("could not reach the instance")?;
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    if !res.status().is_success() {
        bail!("the instance refused a socket ticket ({})", res.status());
    }
    Ok(Some(res.json::<TicketOut>().await.context("unusable ticket reply")?.ticket))
}

#[derive(Deserialize)]
struct ChatOut {
    id: String,
    title: String,
}

/// The chat's title, for a notification. Read from the caller's chat list, which
/// is the listing endpoint the application itself uses; there is no single-chat
/// read to ask instead.
///
/// `None` for anything that goes wrong: a notification with a generic heading is
/// better than none, and far better than a failed request bubbling up somewhere
/// it matters.
pub async fn chat_title(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    chat_id: &str,
) -> Option<String> {
    let res = http.get(format!("{base_url}/api/chats")).bearer_auth(token).send().await;
    let chats: Vec<ChatOut> = res.ok()?.json().await.ok()?;
    chats
        .into_iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.title)
        .filter(|t| !t.trim().is_empty())
}

/// The socket's address, derived from the instance's: a TLS instance gets `wss`.
pub fn ws_url(base_url: &str, query: &str) -> String {
    let ws_base = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{base_url}")
    };
    format!("{ws_base}/ws?{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_gets_the_safe_scheme() {
        assert_eq!(normalise_base("ai.example.com"), "https://ai.example.com");
        assert_eq!(normalise_base(" ai.example.com/ "), "https://ai.example.com");
        assert_eq!(normalise_base("http://localhost:8080"), "http://localhost:8080");
        assert_eq!(normalise_base("https://ai.example.com//"), "https://ai.example.com");
    }

    #[test]
    fn the_socket_follows_the_instance_scheme() {
        assert_eq!(ws_url("https://ai.example.com", "ticket=x"), "wss://ai.example.com/ws?ticket=x");
        assert_eq!(ws_url("http://localhost:8080", "ticket=x"), "ws://localhost:8080/ws?ticket=x");
    }
}

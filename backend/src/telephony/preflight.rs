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

//! Whether a line will actually answer, asked before somebody rings it.
//!
//! Every way a telephone line can be misconfigured has the same symptom from outside: it
//! does not work, and the person who finds out is a caller. The sharpest case is a
//! deployment with no streaming synthesiser, where the line answers, cannot say what it is,
//! and hangs up. That is the right thing to do and a terrible first impression, and it is
//! entirely knowable in advance.
//!
//! So this asks the questions a call asks, in the order a call asks them, and says what to
//! do about each answer. Two rules hold it together.
//!
//! **What is reported is what was found, not what was configured.** A settings page can
//! only say a synthesiser is named. This makes a real request to it, because "named" and
//! "answering" are different facts and only the second one takes a call.
//!
//! **No secret ever appears in a finding.** More people can read this than can set it:
//! whether a credential is stored, never a character of it. There is a test that plants a
//! recognisable secret in the settings and searches every field of every finding for it.

use std::time::Duration;

use serde::Serialize;

use crate::state::AppState;

/// One thing that was looked at.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Check {
    /// A stable name for this finding. The interface keys off it, so it outlives wording.
    pub id: &'static str,
    /// What was being asked, in the words somebody would ask it.
    pub title: &'static str,
    pub ok: bool,
    /// What was found. Never a secret: whether one is stored, never its value.
    pub detail: String,
    /// What to do about it, when it is not right.
    pub fix: Option<String>,
}

impl Check {
    fn good(id: &'static str, title: &'static str, detail: impl Into<String>) -> Self {
        Check { id, title, ok: true, detail: detail.into(), fix: None }
    }

    fn bad(
        id: &'static str,
        title: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Check { id, title, ok: false, detail: detail.into(), fix: Some(fix.into()) }
    }
}

/// How long the synthesiser is given to answer before the check gives up on it.
///
/// Generous compared with what a call can afford, because the question here is whether it
/// works at all rather than whether it is quick. Bounded so that one unreachable engine
/// cannot leave an operator staring at a page that never finishes.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The words sent to the synthesiser to see whether it answers.
///
/// Short, and about the test rather than about anybody: it is spoken to nothing and heard
/// by nobody, but it does travel to whatever engine the deployment has configured.
const PROBE_TEXT: &str = "Testing the telephone line.";

/// Ask every question a call asks, and report what was found.
pub async fn run(state: &AppState) -> Vec<Check> {
    let mut out = Vec::new();
    let cfg = crate::telephony::TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;

    // 1. Is the telephone switched on at all?
    let feature_on = crate::features::enabled_for_user(state, None, "telephony").await;
    out.push(if feature_on {
        Check::good("feature", "The telephone is switched on", "This deployment answers calls.")
    } else {
        Check::bad(
            "feature",
            "The telephone is switched on",
            "It is off, so the whole telephone surface is absent.",
            "Switch the telephone feature on for this deployment.",
        )
    });

    // 2. Is anything named that can answer a call?
    out.push(match cfg.provider.as_str() {
        "twilio" => Check::good("provider", "Something answers calls", "Calls come in through a carrier."),
        "audiosocket" => Check::good(
            "provider",
            "Something answers calls",
            "Calls come in from a telephone system on your own network.",
        ),
        other => Check::bad(
            "provider",
            "Something answers calls",
            if other.is_empty() || other == "none" {
                "Nothing is named, so no call can be taken.".to_string()
            } else {
                format!("{other:?} is not something this deployment can answer with.")
            },
            "Choose a carrier or your own telephone system in the telephone settings.",
        ),
    });

    // 3. Whichever way is named, is it set up?
    if cfg.is_audiosocket() {
        out.extend(own_system(&cfg));
    } else {
        out.extend(carrier(&cfg));
    }

    // 4. Can a credential be stored at all?
    out.push(if state.message_key.is_some() {
        Check::good(
            "message_key",
            "Credentials can be stored safely",
            "An encryption key is configured, so secrets are held encrypted.",
        )
    } else {
        Check::bad(
            "message_key",
            "Credentials can be stored safely",
            "No encryption key is configured, so no telephone credential can be stored.",
            "Set the deployment's message encryption key, then store the credential again.",
        )
    });

    // 5. The engines a call needs, and the synthesiser for real.
    let vc = crate::voice::VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
    out.push(rate_check(vc.stt_sample_rate));
    out.push(synthesiser(state, &vc).await);

    // 6. And the lines themselves.
    out.extend(lines(state).await);
    out
}

fn carrier(cfg: &crate::telephony::TelephonyResolved) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(if cfg.auth_token.is_some() {
        Check::good(
            "carrier_credential",
            "The carrier's credential is stored",
            "A credential is stored, and every request from the carrier is checked against it.",
        )
    } else {
        Check::bad(
            "carrier_credential",
            "The carrier's credential is stored",
            "None is stored, or it could not be read back, so every call will be refused.",
            "Store the carrier's authentication token in the telephone settings.",
        )
    });
    let base = cfg.public_base_url.trim();
    out.push(match crate::telephony::sign::socket_base(base) {
        Some(ws) if !base.is_empty() => Check::good(
            "public_address",
            "The carrier can reach this deployment",
            format!("Calls are answered at {base}, and audio arrives at {ws}."),
        ),
        _ => Check::bad(
            "public_address",
            "The carrier can reach this deployment",
            if base.is_empty() {
                "No public address is set, so the carrier is not told where to send the audio."
                    .to_string()
            } else {
                format!("{base:?} is not an address a carrier can open a connection to.")
            },
            "Set the public address of this deployment, beginning with https, in the telephone settings.",
        ),
    });
    out
}

fn own_system(cfg: &crate::telephony::TelephonyResolved) -> Vec<Check> {
    let mut out = Vec::new();
    let listen = cfg.audiosocket_listen.trim();
    out.push(if listen.is_empty() {
        Check::bad(
            "listen_address",
            "There is somewhere for your telephone system to connect",
            "No address to listen on is set, so nothing is bound and no call can be carried.",
            "Set an address and port to listen on, then restart this deployment.",
        )
    } else if crate::telephony::audiosocket::is_listening() {
        Check::good(
            "listen_address",
            "There is somewhere for your telephone system to connect",
            format!("Listening on {listen}."),
        )
    } else {
        Check::bad(
            "listen_address",
            "There is somewhere for your telephone system to connect",
            format!("{listen} is set, but this deployment is not listening on it."),
            "A listening address is taken up when this deployment starts. Restart it, and if it \
             still does not listen, check that nothing else holds that port.",
        )
    });
    out.push(if cfg.audiosocket_key.is_some() {
        Check::good(
            "shared_secret",
            "Your telephone system can identify itself",
            "A shared secret is stored, and requests are checked against it.",
        )
    } else {
        Check::bad(
            "shared_secret",
            "Your telephone system can identify itself",
            "None is stored, or it could not be read back, so every request will be refused.",
            "Store a shared secret in the telephone settings and put the same one in your telephone \
             system's call routing.",
        )
    });
    out
}

/// A telephone line carries narrowband audio, and only two recognition rates can be
/// reached from it without inventing samples.
fn rate_check(rate: u32) -> Check {
    match rate.max(8_000) {
        8_000 | 16_000 => Check::good(
            "recognition_rate",
            "Recognition can take what a telephone carries",
            format!("Audio is converted to {} Hz for recognition.", rate.max(8_000)),
        ),
        other => Check::bad(
            "recognition_rate",
            "Recognition can take what a telephone carries",
            format!("Recognition is set to {other} Hz, which a telephone line cannot be converted to."),
            "Set the recognition sample rate to 16000, or to 8000 to leave the audio as the line \
             carries it.",
        ),
    }
}

/// Ask the synthesiser to say something, and see whether it does.
///
/// The one check that makes a real request, and the reason this whole thing exists: a
/// deployment whose synthesiser is absent or refusing answers calls and hangs up on them,
/// which looks from the outside exactly like a line that does not work.
async fn synthesiser(state: &AppState, vc: &crate::voice::VoiceLiveResolved) -> Check {
    const ID: &'static str = "synthesiser";
    const TITLE: &'static str = "The line can speak";
    if !vc.tts_stream || vc.tts_stream_url.trim().is_empty() {
        return Check::bad(
            ID,
            TITLE,
            "No streaming synthesiser is configured. A call would be answered and then ended, \
             because nothing here can turn a reply into audio for a telephone.",
            "Configure a streaming speech synthesiser and switch streaming synthesis on.",
        );
    }
    let client = reqwest::Client::new();
    let voice = (!vc.tts_voice.is_empty()).then_some(vc.tts_voice.as_str());
    let opened = tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::voice::tts_stream::stream_clause(
            &client,
            &vc.tts_stream_url,
            &vc.tts_model,
            PROBE_TEXT,
            voice,
            vc.tts_api_key.as_deref(),
            crate::voice::tts_stream::ClauseFormat::Pcm24k,
        ),
    )
    .await;
    let mut stream = match opened {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Check::bad(
                ID,
                TITLE,
                format!("The synthesiser refused: {e}"),
                "Check the speech synthesiser's address, model and credential in the voice settings.",
            )
        }
        Err(_) => {
            return Check::bad(
                ID,
                TITLE,
                format!("The synthesiser did not answer within {} seconds.", PROBE_TIMEOUT.as_secs()),
                "Check that the speech synthesiser is running and reachable from this deployment.",
            )
        }
    };
    // Bounded the same way: an engine that opens and then says nothing is as unusable as
    // one that refuses, and must not leave this waiting.
    let mut bytes = 0usize;
    let read = tokio::time::timeout(PROBE_TIMEOUT, async {
        while let Some(chunk) = stream.recv().await {
            bytes += chunk.len();
            // Enough to know it is producing audio. The rest of the sentence is not
            // needed and is somebody's compute.
            if bytes >= 4_096 {
                break;
            }
        }
    })
    .await;
    let _ = state;
    match read {
        Ok(()) if bytes > 0 => Check::good(
            ID,
            TITLE,
            format!("The synthesiser answered with {bytes} bytes of audio for a test phrase."),
        ),
        Ok(()) => Check::bad(
            ID,
            TITLE,
            "The synthesiser accepted the request and returned no audio.",
            "Check the synthesiser's model name: a wrong one is often accepted and then produces \
             nothing.",
        ),
        Err(_) => Check::bad(
            ID,
            TITLE,
            "The synthesiser started answering and then stopped.",
            "Check that the speech synthesiser is healthy and not overloaded.",
        ),
    }
}

/// Are there lines, and can each of them take a call?
async fn lines(state: &AppState) -> Vec<Check> {
    let rows = sqlx::query!(
        r#"SELECT p.e164, p.enabled, p.provider,
                  (a.archived_at IS NOT NULL) AS "agent_archived!",
                  (u.deactivated_at IS NOT NULL) AS "owner_gone!"
             FROM phone_numbers p
             JOIN agents a ON a.id = p.agent_id
             JOIN users u ON u.id = p.owner_user_id
            ORDER BY p.e164"#
    )
    .fetch_all(&state.pg)
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            return vec![Check::bad(
                "lines",
                "There is a number to ring",
                format!("The lines could not be read: {e}"),
                "Check that the database is reachable.",
            )]
        }
    };
    if rows.is_empty() {
        return vec![Check::bad(
            "lines",
            "There is a number to ring",
            "No line is registered, so there is no number that reaches this deployment.",
            "Register a number, bind it to an agent and an account, and switch it on.",
        )];
    }
    let answering = rows.iter().filter(|r| r.enabled).count();
    let mut out = vec![if answering > 0 {
        Check::good(
            "lines",
            "There is a number to ring",
            format!("{answering} of {} registered lines are switched on.", rows.len()),
        )
    } else {
        Check::bad(
            "lines",
            "There is a number to ring",
            format!("{} lines are registered and none is switched on.", rows.len()),
            "Switch a line on once you have checked what its agent can reach.",
        )
    }];

    // A line pointing at an archived agent or a closed account answers and then fails,
    // which is the same invisible fault as the rest of this list.
    let broken: Vec<String> = rows
        .iter()
        .filter(|r| r.enabled && (r.agent_archived || r.owner_gone))
        .map(|r| {
            let why = if r.agent_archived { "its agent has been archived" } else { "its account is closed" };
            format!("{} ({why})", r.e164)
        })
        .collect();
    if !broken.is_empty() {
        out.push(Check::bad(
            "line_bindings",
            "Every line that is on can take a call",
            format!("These are switched on and cannot answer: {}.", broken.join(", ")),
            "Bind each of those lines to an agent that exists and an account that is open, or switch \
             the line off.",
        ));
    } else if answering > 0 {
        out.push(Check::good(
            "line_bindings",
            "Every line that is on can take a call",
            "Each line that is switched on has an agent and an account behind it.",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interface keys off these, so they outlive any wording and must not collide.
    #[test]
    fn every_finding_has_its_own_stable_name() {
        let ids = [
            "feature",
            "provider",
            "carrier_credential",
            "public_address",
            "listen_address",
            "shared_secret",
            "message_key",
            "recognition_rate",
            "synthesiser",
            "lines",
            "line_bindings",
        ];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "two findings share a name");
    }

    /// A finding that is not right always says what to do about it. One without is a page
    /// that tells an operator they have a problem and leaves them there.
    #[test]
    fn anything_wrong_says_what_to_do() {
        let bad = Check::bad("x", "X", "it is wrong", "put it right");
        assert!(!bad.ok);
        assert!(bad.fix.is_some());
        let good = Check::good("x", "X", "it is right");
        assert!(good.ok);
        assert!(good.fix.is_none(), "nothing to fix when nothing is wrong");
    }

    /// The two rates a telephone line can reach, and the arithmetic behind why.
    #[test]
    fn only_the_rates_a_telephone_can_reach_pass() {
        assert!(rate_check(8_000).ok);
        assert!(rate_check(16_000).ok);
        // Unset means the line's own rate, which is fine.
        assert!(rate_check(0).ok);
        for wrong in [22_050, 24_000, 44_100, 48_000] {
            let c = rate_check(wrong);
            assert!(!c.ok, "{wrong} Hz should not pass");
            assert!(c.fix.as_deref().unwrap_or_default().contains("16000"));
        }
    }

    /// The whole point of the carrier findings: what is reported is whether a credential
    /// exists, never the credential.
    #[test]
    fn a_credential_is_reported_as_present_and_never_shown() {
        let secret = "super-secret-carrier-token-9f3b";
        let cfg = crate::telephony::TelephonyResolved {
            provider: "twilio".into(),
            public_base_url: "https://calls.example.test".into(),
            max_concurrent_calls: 2,
            auth_token: Some(secret.into()),
            audiosocket_listen: String::new(),
            audiosocket_key: None,
        };
        let checks = carrier(&cfg);
        assert!(checks.iter().all(|c| c.ok), "a complete carrier setup has nothing wrong");
        for c in &checks {
            let text = format!("{} {} {}", c.title, c.detail, c.fix.clone().unwrap_or_default());
            assert!(!text.contains(secret), "a credential appeared in {:?}", c.id);
        }
    }

    /// And the same for the other path's secret.
    #[test]
    fn a_shared_secret_is_reported_as_present_and_never_shown() {
        let secret = "shared-secret-for-the-pbx-77aa";
        let mut cfg = crate::telephony::TelephonyResolved {
            provider: "audiosocket".into(),
            public_base_url: String::new(),
            max_concurrent_calls: 2,
            auth_token: None,
            audiosocket_listen: "0.0.0.0:9092".into(),
            audiosocket_key: Some(secret.into()),
        };
        for c in own_system(&cfg) {
            let text = format!("{} {} {}", c.title, c.detail, c.fix.clone().unwrap_or_default());
            assert!(!text.contains(secret), "a secret appeared in {:?}", c.id);
        }
        // With nothing set, both findings are wrong and both say what to do.
        cfg.audiosocket_listen = String::new();
        cfg.audiosocket_key = None;
        let checks = own_system(&cfg);
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| !c.ok && c.fix.is_some()));
    }

    /// A public address the carrier could not open a connection to is a call that never
    /// carries audio, and the address is the thing an operator most often gets wrong.
    #[test]
    fn a_public_address_has_to_be_one_a_carrier_can_reach() {
        let with = |base: &str| crate::telephony::TelephonyResolved {
            provider: "twilio".into(),
            public_base_url: base.into(),
            max_concurrent_calls: 2,
            auth_token: Some("t".into()),
            audiosocket_listen: String::new(),
            audiosocket_key: None,
        };
        let ok = carrier(&with("https://calls.example.test"));
        assert!(ok.iter().find(|c| c.id == "public_address").unwrap().ok);
        for wrong in ["", "calls.example.test", "ftp://calls.example.test"] {
            let bad = carrier(&with(wrong));
            let c = bad.iter().find(|c| c.id == "public_address").unwrap();
            assert!(!c.ok, "{wrong:?} should not pass");
        }
    }
}

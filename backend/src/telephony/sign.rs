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

//! Proving a webhook really came from the carrier.
//!
//! The carrier's endpoints are the first surfaces in this product that anyone on the
//! internet can reach without an account, so the request signature is the whole of
//! their authentication. A request that does not carry a valid one is not a call, and
//! is refused before anything reads its contents.
//!
//! The scheme is the carrier's, not ours: HMAC-SHA1 over the request URL followed by
//! every form parameter, sorted by name and concatenated with no separators at all.
//!
//! The URL is the part that goes wrong in practice, and it is taken from
//! configuration rather than from the request. Behind a reverse proxy this process
//! sees a plain local address while the carrier signed the public one, so a URL built
//! from what arrived would never match. Forwarded headers would appear to fix that,
//! but they can be set by whoever is calling whenever the proxy does not strip them,
//! and a caller who could choose the string being signed gains nothing anyway
//! (without the token they still cannot sign it) while an operator loses the ability
//! to know what the check is actually comparing. So it is configured, it must match
//! the carrier's console exactly, and if it is missing the request is refused rather
//! than checked against a guess.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// The header the signature arrives in.
pub const SIGNATURE_HEADER: &str = "X-Twilio-Signature";

/// Compute the signature a carrier would put on this request.
///
/// `params` are the form pairs with their values already decoded. Order does not
/// matter: they are sorted here, by name, bytewise.
pub fn signature(auth_token: &str, url: &str, params: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut signed = String::with_capacity(url.len() + 64);
    signed.push_str(url);
    for (key, value) in sorted {
        signed.push_str(key);
        signed.push_str(value);
    }
    let mut mac =
        Hmac::<Sha1>::new_from_slice(auth_token.as_bytes()).expect("hmac takes a key of any length");
    mac.update(signed.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

/// Does `presented` match what the carrier should have sent?
///
/// An absent token or an empty header is a refusal rather than a comparison: a
/// signature that cannot be checked has not been checked.
pub fn verify(auth_token: &str, url: &str, params: &[(String, String)], presented: &str) -> bool {
    if auth_token.is_empty() || presented.is_empty() {
        return false;
    }
    let expected = signature(auth_token, url, params);
    crate::http::ct_eq(expected.as_bytes(), presented.as_bytes())
}

/// Decode a form body into its pairs, keeping duplicates and order.
///
/// Done by hand rather than through a typed extractor for two reasons: the extractor
/// consumes the body the signature is computed over, and a struct would quietly drop
/// parameters it does not know about. The carrier adds parameters over time, and one
/// dropped parameter is a signature that never matches again.
pub fn form_pairs(body: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(body.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// Look one parameter up. First occurrence wins.
pub fn param<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The URL the carrier signed: the configured public base, the route's path, and the
/// query string exactly as it arrived.
pub fn signed_url(base: &str, path: &str, query: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    match query.filter(|q| !q.is_empty()) {
        Some(q) => format!("{base}{path}?{q}"),
        None => format!("{base}{path}"),
    }
}

/// Is there enough of a base URL to build the string a carrier signed?
///
/// This is the one that fails closed, and it is deliberately the narrow question. A
/// signature computed over nothing has not been checked, so an absent or scheme-less
/// base is a refusal. A base that is merely *wrong* is a different matter: the
/// signature simply will not match, which is already a refusal with an audit row
/// explaining it, and second-guessing which addresses a carrier could have reached
/// would mean guessing at somebody's network.
pub fn base_is_usable(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("https://").or_else(|| base.strip_prefix("http://")) else {
        return false;
    };
    !rest.split(['/', '?']).next().unwrap_or("").is_empty()
}

/// Does this base name only this machine?
///
/// Almost always a sign that the public address was never configured and the fallback
/// was used, in which case every request will fail its signature check and the line
/// will ring and never answer. Worth saying out loud, but not worth refusing over: a
/// proxy on the same host, or a test, is a legitimate way to arrive at one.
pub fn base_is_local(base: &str) -> bool {
    let rest = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or(base);
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1")
}

/// Turn the public base into the scheme a media socket is opened with.
///
/// A carrier refuses to open a plaintext socket, so in a real deployment this is
/// always the secure one. It is derived rather than fixed so that a test, which is
/// the only thing that ever serves plain HTTP, can still reach it.
pub fn socket_base(base: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("https://") {
        return Some(format!("wss://{rest}"));
    }
    base.strip_prefix("http://").map(|rest| format!("ws://{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from the carrier's own documentation. This is the only test
    /// here that proves the algorithm rather than its properties: everything else
    /// would pass just as well against a consistently wrong implementation.
    const DOC_URL: &str = "https://example.com/myapp.php?foo=1&bar=2";
    const DOC_TOKEN: &str = "12345";
    const DOC_SIGNATURE: &str = "L/OH5YylLD5NRKLltdqwSvS0BnU=";

    fn doc_params() -> Vec<(String, String)> {
        [
            ("CallSid", "CA1234567890ABCDE"),
            ("Caller", "+14158675310"),
            ("Digits", "1234"),
            ("From", "+14158675310"),
            ("To", "+18005551212"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn the_documented_example_signs_to_the_documented_value() {
        assert_eq!(signature(DOC_TOKEN, DOC_URL, &doc_params()), DOC_SIGNATURE);
        assert!(verify(DOC_TOKEN, DOC_URL, &doc_params(), DOC_SIGNATURE));
    }

    /// Order is not part of the signature, so a carrier reordering its body must not
    /// break the check.
    #[test]
    fn the_order_the_parameters_arrive_in_does_not_matter() {
        let mut shuffled = doc_params();
        shuffled.reverse();
        assert_eq!(signature(DOC_TOKEN, DOC_URL, &shuffled), DOC_SIGNATURE);
    }

    /// Every part of the input is part of the signature. Each of these is a way an
    /// attacker might hope to reuse a signature they have seen.
    #[test]
    fn changing_anything_at_all_changes_the_signature() {
        let base = signature(DOC_TOKEN, DOC_URL, &doc_params());

        let mut extra = doc_params();
        extra.push(("Extra".into(), "1".into()));
        assert_ne!(signature(DOC_TOKEN, DOC_URL, &extra), base, "an added parameter");

        let mut edited = doc_params();
        edited[0].1 = "CA0000000000ABCDE".into();
        assert_ne!(signature(DOC_TOKEN, DOC_URL, &edited), base, "an edited value");

        let mut renamed = doc_params();
        renamed[0].0 = "CallSid2".into();
        assert_ne!(signature(DOC_TOKEN, DOC_URL, &renamed), base, "a renamed parameter");

        assert_ne!(
            signature(DOC_TOKEN, "https://example.com/myapp.php?foo=1", &doc_params()),
            base,
            "a different query string"
        );
        assert_ne!(signature("12346", DOC_URL, &doc_params()), base, "a different token");
    }

    #[test]
    fn a_near_miss_is_still_a_miss() {
        // Same length, one character out. The comparison has to look at all of it: a
        // check that stopped early, or one that only measured the length, would let
        // this through.
        let mut wrong: Vec<char> = DOC_SIGNATURE.chars().collect();
        wrong[0] = if wrong[0] == 'A' { 'B' } else { 'A' };
        let wrong: String = wrong.into_iter().collect();
        assert_ne!(wrong, DOC_SIGNATURE, "the fixture must actually differ");
        assert!(!verify(DOC_TOKEN, DOC_URL, &doc_params(), &wrong));

        // And a prefix of the right answer is not the right answer.
        assert!(!verify(DOC_TOKEN, DOC_URL, &doc_params(), &DOC_SIGNATURE[..10]));
    }

    #[test]
    fn nothing_to_check_with_is_a_refusal() {
        assert!(!verify("", DOC_URL, &doc_params(), DOC_SIGNATURE), "no token");
        assert!(!verify(DOC_TOKEN, DOC_URL, &doc_params(), ""), "no signature");
    }

    /// A number arrives in a form body with its leading plus escaped, and the
    /// signature is over the decoded value. Decoding it as a query string instead
    /// would read that escape as a space and never match.
    #[test]
    fn a_form_body_decodes_to_the_values_that_get_signed() {
        let body = "CallSid=CA1234567890ABCDE&Caller=%2B14158675310&Digits=1234\
                    &From=%2B14158675310&To=%2B18005551212";
        let pairs = form_pairs(body);
        assert_eq!(param(&pairs, "Caller"), Some("+14158675310"));
        assert_eq!(signature(DOC_TOKEN, DOC_URL, &pairs), DOC_SIGNATURE);
    }

    #[test]
    fn a_repeated_parameter_is_kept_rather_than_collapsed() {
        let pairs = form_pairs("A=1&A=2");
        assert_eq!(pairs.len(), 2);
        assert_eq!(param(&pairs, "A"), Some("1"));
    }

    #[test]
    fn the_signed_url_is_built_from_configuration() {
        assert_eq!(
            signed_url("https://calls.example.com/", "/api/telephony/twilio/voice", None),
            "https://calls.example.com/api/telephony/twilio/voice"
        );
        assert_eq!(
            signed_url("https://calls.example.com", "/x", Some("a=1&b=2")),
            "https://calls.example.com/x?a=1&b=2"
        );
        assert_eq!(signed_url("https://calls.example.com", "/x", Some("")), "https://calls.example.com/x");
    }

    /// A base with nothing in it cannot be signed over, so it is refused. A base that
    /// is merely unlikely is not: that shows up as a signature that does not match.
    #[test]
    fn a_base_that_cannot_be_signed_over_is_refused() {
        assert!(base_is_usable("https://calls.example.com"));
        assert!(base_is_usable("http://198.51.100.7:8080"));
        assert!(base_is_usable("http://127.0.0.1:8080"), "a local address is usable, if odd");
        assert!(!base_is_usable(""));
        assert!(!base_is_usable("calls.example.com"), "a base with no scheme");
        assert!(!base_is_usable("https://"), "a scheme with no host");
    }

    /// Naming only this machine is worth warning about, because it is what an
    /// unconfigured public address falls back to.
    #[test]
    fn a_base_naming_only_this_machine_is_recognised() {
        assert!(base_is_local("http://localhost:8088"));
        assert!(base_is_local("http://127.0.0.1:8080"));
        assert!(base_is_local("https://0.0.0.0"));
        assert!(!base_is_local("https://calls.example.com"));
        assert!(!base_is_local("http://198.51.100.7:8080"));
    }

    #[test]
    fn the_socket_scheme_follows_the_base() {
        assert_eq!(socket_base("https://calls.example.com/").as_deref(), Some("wss://calls.example.com"));
        assert_eq!(socket_base("http://127.0.0.1:8080").as_deref(), Some("ws://127.0.0.1:8080"));
        assert_eq!(socket_base("calls.example.com"), None);
    }
}

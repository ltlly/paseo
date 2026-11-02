//! Relay-only pairing offers and `relay://` connection URIs
//! (`@getpaseo/protocol/connection-offer`, `daemon-endpoints`).

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::endpoints::{normalize_host_port, parse_host_port, validated_endpoint};

pub const SECURITY_MODE_PROTECTED: &str = "protected";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionOffer {
    pub v: u32,
    pub server_id: String,
    pub daemon_public_key_b64: String,
    pub relay: OfferRelay,
    /// Present when the daemon requires protected endpoint security; clients
    /// pin it before their first handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_security_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferRelay {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_tls: Option<bool>,
}

fn decode_base64url_to_utf8(input: &str) -> Result<String> {
    let normalized: String = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .map(|byte| match byte {
            b'-' => b'+',
            b'_' => b'/',
            other => other,
        })
        .map(|byte| byte as char)
        .collect();
    let padded_len = normalized.len().div_ceil(4) * 4;
    let padded = format!("{normalized:=<padded_len$}");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded.as_bytes())
        .context("Invalid offer encoding")?;
    String::from_utf8(bytes).context("Offer payload is not UTF-8")
}

fn validate_offer(offer: ConnectionOffer) -> Result<ConnectionOffer> {
    if offer.v != 2 {
        bail!("Unsupported pairing offer version");
    }
    if offer.server_id.is_empty() || offer.daemon_public_key_b64.is_empty() {
        bail!("Pairing offer is incomplete");
    }
    // A pairing offer must never publish a credential-bearing relay authority.
    normalize_host_port(&offer.relay.endpoint)?;
    validated_endpoint(&normalize_host_port(&offer.relay.endpoint)?)?;
    Ok(offer)
}

const OFFER_FRAGMENT_PREFIX: &str = "#offer=";
const CONNECT_FRAGMENT_PREFIX: &str = "#connect=";

/// Parses a pairing-offer URL such as `https://app.paseo.sh/#offer=<base64url>`.
/// Returns `Ok(None)` when the input has no offer fragment.
pub fn parse_connection_offer_from_url(input: &str) -> Result<Option<ConnectionOffer>> {
    let trimmed = input.trim();
    let Some(index) = trimmed.find(OFFER_FRAGMENT_PREFIX) else {
        return Ok(None);
    };
    let encoded = trimmed[index + OFFER_FRAGMENT_PREFIX.len()..].trim();
    if encoded.is_empty() {
        return Ok(None);
    }
    let json = decode_base64url_to_utf8(encoded)?;
    let offer: ConnectionOffer =
        serde_json::from_str(&json).context("Invalid pairing offer payload")?;
    Ok(Some(validate_offer(offer)?))
}

pub struct ParsedRelayConnectionUri {
    pub offer: ConnectionOffer,
    pub password: Option<String>,
}

/// Percent-decoding with `+` treated as space, matching `URLSearchParams.get`.
fn form_urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&String::from_utf8_lossy(&bytes[index + 1..index + 3]), 16)
                {
                    Ok(byte) => {
                        decoded.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        decoded.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// `decodeURIComponent` without plus-to-space, matching pathname decoding.
fn uri_component_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&String::from_utf8_lossy(&bytes[index + 1..index + 3]), 16)
                {
                    Ok(byte) => {
                        decoded.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        decoded.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

pub fn parse_relay_connection_uri(input: &str) -> Result<ParsedRelayConnectionUri> {
    let trimmed = input.trim();
    let uri = match trimmed.find(CONNECT_FRAGMENT_PREFIX) {
        Some(index) => form_urldecode(trimmed[index + CONNECT_FRAGMENT_PREFIX.len()..].trim()),
        None => trimmed.to_owned(),
    };
    let rest = uri
        .strip_prefix("relay://")
        .ok_or_else(|| anyhow::format_err!("Invalid relay connection URI"))?;
    if rest.contains('#') {
        bail!("Invalid relay connection URI");
    }
    let (authority_and_path, query) = match rest.find('?') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, ""),
    };
    // Hostname and port must be present; userinfo is never supported.
    let (authority, path) = match authority_and_path.find('/') {
        Some(index) => (
            &authority_and_path[..index],
            &authority_and_path[index + 1..],
        ),
        None => bail!("Invalid relay connection URI"),
    };
    if authority.is_empty() || authority.contains('@') {
        bail!("Invalid relay connection URI");
    }
    let endpoint = normalize_host_port(authority)?;
    parse_host_port(&endpoint)?;

    let mut key: Option<String> = None;
    let mut use_tls = false;
    let mut password: Option<String> = None;
    let mut security: Option<String> = None;
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        match name {
            "key" => key = Some(form_urldecode(value)),
            "ssl" => use_tls = value == "true",
            "password" => password = Some(form_urldecode(value)),
            "security" => security = Some(form_urldecode(value)),
            _ => {}
        }
    }
    let server_id = uri_component_decode(path);
    let key = key
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow::format_err!("Relay connection URI requires a server id and key"))?;
    if server_id.is_empty() || server_id.contains('/') {
        bail!("Relay connection URI requires a server id and key");
    }
    match security.as_deref() {
        None | Some(SECURITY_MODE_PROTECTED) => {}
        Some(_) => bail!("Relay connection URI has an unsupported security mode"),
    }
    Ok(ParsedRelayConnectionUri {
        offer: validate_offer(ConnectionOffer {
            v: 2,
            server_id,
            daemon_public_key_b64: key,
            relay: OfferRelay {
                endpoint,
                use_tls: use_tls.then_some(true),
            },
            minimum_security_mode: security.filter(|mode| mode == SECURITY_MODE_PROTECTED),
        })?,
        password: password.filter(|password| !password.is_empty()),
    })
}

/// Percent-encodes a query value the way `URLSearchParams.set` does, so a
/// relay server's URL parser reads back the same value.
fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push_str("%20"),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn uri_component_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

pub fn serialize_relay_connection_uri(parts: &ParsedRelayConnectionUri) -> String {
    let offer = &parts.offer;
    let mut uri = format!(
        "relay://{}/{}?key={}",
        offer.relay.endpoint,
        uri_component_encode(&offer.server_id),
        encode_query_component(&offer.daemon_public_key_b64)
    );
    if offer.relay.use_tls == Some(true) {
        uri.push_str("&ssl=true");
    }
    if offer.minimum_security_mode.is_some() {
        uri.push_str("&security=protected");
    }
    if let Some(password) = &parts.password {
        uri.push_str("&password=");
        uri.push_str(&encode_query_component(password));
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer_json(endpoint: &str) -> String {
        format!(
            r#"{{"v":2,"serverId":"srv_test","daemonPublicKeyB64":"private-key","relay":{{"endpoint":"{endpoint}"}}}}"#
        )
    }

    #[test]
    fn decodes_base64url_json_payloads() {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(offer_json("relay.paseo.sh:443"));
        let offer =
            parse_connection_offer_from_url(&format!("https://app.paseo.sh/#offer={encoded}"))
                .unwrap()
                .unwrap();
        assert_eq!(offer.server_id, "srv_test");
        assert_eq!(offer.daemon_public_key_b64, "private-key");
        assert_eq!(offer.relay.endpoint, "relay.paseo.sh:443");
        assert_eq!(offer.relay.use_tls, None);
        assert_eq!(offer.minimum_security_mode, None);
    }

    #[test]
    fn returns_none_without_an_offer_fragment() {
        assert!(
            parse_connection_offer_from_url("https://app.paseo.sh/")
                .unwrap()
                .is_none()
        );
        assert!(
            parse_connection_offer_from_url("https://app.paseo.sh/#offer=")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pairing_offers_cannot_publish_credential_bearing_or_malformed_relay_authorities() {
        for endpoint in [
            "alice:example-secret@localhost:6767",
            "localhost/path:6767",
            "localhost?token=example-secret:6767",
        ] {
            let encoded =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(offer_json(endpoint));
            assert!(
                parse_connection_offer_from_url(&format!("https://app.paseo.sh/#offer={encoded}"))
                    .is_err(),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn round_trips_the_offer_and_password_through_a_relay_uri() {
        let parts = ParsedRelayConnectionUri {
            offer: ConnectionOffer {
                v: 2,
                server_id: "srv_test".into(),
                daemon_public_key_b64: "private-key".into(),
                relay: OfferRelay {
                    endpoint: "relay.paseo.sh:443".into(),
                    use_tls: Some(true),
                },
                minimum_security_mode: None,
            },
            password: Some("secret".into()),
        };
        let uri = serialize_relay_connection_uri(&parts);
        let parsed = parse_relay_connection_uri(&uri).unwrap();
        assert_eq!(parsed.offer, parts.offer);
        assert_eq!(parsed.password.as_deref(), Some("secret"));
    }

    #[test]
    fn round_trips_a_protected_minimum_security_mode() {
        let uri = "relay://relay.example.com:443/srv_x?key=k&security=protected";
        let parsed = parse_relay_connection_uri(uri).unwrap();
        assert_eq!(
            parsed.offer.minimum_security_mode.as_deref(),
            Some(SECURITY_MODE_PROTECTED)
        );
    }

    #[test]
    fn rejects_unknown_security_modes_instead_of_downgrading() {
        assert!(parse_relay_connection_uri("relay://r:443/s?key=k&security=legacy").is_err());
    }

    #[test]
    fn rejects_incomplete_or_credential_bearing_relay_uris() {
        for uri in [
            "relay://alice:secret@localhost:6767/server?key=private",
            "relay://localhost/server?key=private",
            "relay://localhost:6767?key=private",
            "relay://localhost:6767/?key=",
            "relay://localhost:6767//key?key=private",
            "http://localhost:6767/server?key=private",
        ] {
            assert!(parse_relay_connection_uri(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn parses_the_connect_fragment_wrapper() {
        let parsed = parse_relay_connection_uri(
            "https://app.paseo.sh/#connect=relay%3A%2F%2Frelay.example.com%3A443%2Fsrv_x%3Fkey%3Dk",
        )
        .unwrap();
        assert_eq!(parsed.offer.server_id, "srv_x");
        assert_eq!(parsed.offer.relay.endpoint, "relay.example.com:443");
        assert_eq!(parsed.offer.daemon_public_key_b64, "k");
    }
}

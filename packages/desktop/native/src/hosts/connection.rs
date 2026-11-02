//! Saved host connection model mirroring `packages/app/src/types/host-connection.ts`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::endpoints::{
    encode_query_component, normalize_host_port, normalize_loopback_to_localhost,
    should_use_tls_for_default_hosted_relay,
};
use super::offer::ConnectionOffer;
use super::target::{DEFAULT_SSH_DAEMON_PORT, HostTarget, validate_port, validate_ssh_host};

pub const SECURITY_MODE_PROTECTED: &str = "protected";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostConnection {
    #[serde(rename = "routed")]
    Routed {
        id: String,
        origin_server_id: String,
    },
    #[serde(rename = "directTcp")]
    DirectTcp {
        id: String,
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        use_tls: Option<bool>,
    },
    #[serde(rename = "directSocket")]
    DirectSocket { id: String, path: String },
    #[serde(rename = "directPipe")]
    DirectPipe { id: String, path: String },
    #[serde(rename = "remoteSsh")]
    RemoteSsh {
        id: String,
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh_port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        daemon_port: Option<u16>,
    },
    #[serde(rename = "relay")]
    Relay {
        id: String,
        relay_endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        use_tls: Option<bool>,
        daemon_public_key_b64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum_security_mode: Option<String>,
    },
}

impl HostConnection {
    /// Stable identity shared with the app grammar: the id is derived from the
    /// connection facts, never from a user label.
    pub fn id(&self) -> &str {
        match self {
            HostConnection::Routed { id, .. }
            | HostConnection::DirectTcp { id, .. }
            | HostConnection::DirectSocket { id, .. }
            | HostConnection::DirectPipe { id, .. }
            | HostConnection::RemoteSsh { id, .. }
            | HostConnection::Relay { id, .. } => id,
        }
    }

    pub fn relay_daemon_key(&self) -> Option<&str> {
        match self {
            HostConnection::Relay {
                daemon_public_key_b64,
                ..
            } => Some(daemon_public_key_b64),
            _ => None,
        }
    }

    /// A saved relay route as a concrete `relay://` connection URI the shared
    /// grammar parses back: endpoint, serverId, daemon key, password and
    /// security floor travel together, so clicking a saved relay host
    /// reconstructs the exact protected target instead of an empty grammar.
    /// Non-relay connections return `None`.
    pub fn relay_connection_uri(&self, server_id: &str, password: Option<&str>) -> Option<String> {
        let HostConnection::Relay {
            id: _,
            relay_endpoint,
            use_tls,
            daemon_public_key_b64,
            minimum_security_mode,
        } = self
        else {
            return None;
        };
        Some(super::offer::serialize_relay_connection_uri(
            &super::offer::ParsedRelayConnectionUri {
                offer: super::offer::ConnectionOffer {
                    v: 2,
                    server_id: server_id.to_owned(),
                    daemon_public_key_b64: daemon_public_key_b64.clone(),
                    relay: super::offer::OfferRelay {
                        endpoint: relay_endpoint.clone(),
                        use_tls: *use_tls,
                    },
                    minimum_security_mode: minimum_security_mode.clone(),
                },
                password: password
                    .filter(|password| !password.is_empty())
                    .map(str::to_owned),
            },
        ))
    }

    /// Identity equality: same type and same facts. The relay security pin is
    /// policy on top of identity and deliberately not compared here.
    pub fn equals_identity(&self, other: &HostConnection) -> bool {
        match (self, other) {
            (
                HostConnection::Routed {
                    origin_server_id: a,
                    ..
                },
                HostConnection::Routed {
                    origin_server_id: b,
                    ..
                },
            ) => a == b,
            (
                HostConnection::DirectTcp {
                    endpoint: a,
                    use_tls: ta,
                    ..
                },
                HostConnection::DirectTcp {
                    endpoint: b,
                    use_tls: tb,
                    ..
                },
            ) => a == b && ta.unwrap_or(false) == tb.unwrap_or(false),
            (
                HostConnection::DirectSocket { path: a, .. },
                HostConnection::DirectSocket { path: b, .. },
            ) => a == b,
            (
                HostConnection::DirectPipe { path: a, .. },
                HostConnection::DirectPipe { path: b, .. },
            ) => a == b,
            (
                HostConnection::RemoteSsh {
                    host: ha,
                    ssh_port: sa,
                    daemon_port: da,
                    ..
                },
                HostConnection::RemoteSsh {
                    host: hb,
                    ssh_port: sb,
                    daemon_port: db,
                    ..
                },
            ) => ha == hb && sa == sb && da == db,
            (
                HostConnection::Relay {
                    relay_endpoint: ea,
                    use_tls: ta,
                    daemon_public_key_b64: ka,
                    ..
                },
                HostConnection::Relay {
                    relay_endpoint: eb,
                    use_tls: tb,
                    daemon_public_key_b64: kb,
                    ..
                },
            ) => ea == eb && ta == tb && ka == kb,
            _ => false,
        }
    }
}

/// A daemon `listen` value becomes a saved local connection candidate.
pub fn connection_from_listen(listen: &str) -> Option<HostConnection> {
    let normalized = listen.trim();
    if normalized.is_empty() {
        return None;
    }
    if let Some(path) = normalized.strip_prefix("pipe://") {
        let path = path.trim();
        return (!path.is_empty()).then(|| HostConnection::DirectPipe {
            id: format!("pipe:{path}"),
            path: path.to_owned(),
        });
    }
    if let Some(path) = normalized.strip_prefix("unix://") {
        let path = path.trim();
        return (!path.is_empty()).then(|| HostConnection::DirectSocket {
            id: format!("socket:{path}"),
            path: path.to_owned(),
        });
    }
    if normalized.starts_with("\\\\.\\pipe\\") {
        return Some(HostConnection::DirectPipe {
            id: format!("pipe:{normalized}"),
            path: normalized.to_owned(),
        });
    }
    if normalized.starts_with('/') {
        return Some(HostConnection::DirectSocket {
            id: format!("socket:{normalized}"),
            path: normalized.to_owned(),
        });
    }
    let endpoint = normalize_loopback_to_localhost(&normalize_host_port(normalized).ok()?).ok()?;
    Some(HostConnection::DirectTcp {
        id: format!("direct:{endpoint}"),
        endpoint,
        use_tls: None,
    })
}

/// Saves a parsed grammar target when the connection model can express every
/// fact that changes the destination. A WebSocket URL with a custom path or
/// query cannot: a saved `directTcp` always reconnects at `/ws`, so storing it
/// would silently connect somewhere else.
pub fn connection_from_target(target: &HostTarget) -> Option<HostConnection> {
    match target {
        HostTarget::Ssh { ssh, .. } => create_remote_ssh_host_connection(SshConnectionInput {
            host: &ssh.host,
            ssh_port: ssh.ssh_port,
            daemon_port: Some(ssh.daemon_port),
        })
        .ok(),
        HostTarget::Socket {
            socket_path: Some(path),
            pipe,
            ..
        } => Some(if *pipe {
            HostConnection::DirectPipe {
                id: format!("pipe:{path}"),
                path: path.clone(),
            }
        } else {
            HostConnection::DirectSocket {
                id: format!("socket:{path}"),
                path: path.clone(),
            }
        }),
        HostTarget::Socket {
            socket_path: None,
            offer: Some(offer),
            ..
        } => Some(relay_connection_from_offer(offer)),
        HostTarget::Socket {
            socket_path: None,
            url,
            ..
        } => {
            let (endpoint, use_tls) = websocket_endpoint(url)?;
            Some(HostConnection::DirectTcp {
                id: format!("direct:{endpoint}"),
                endpoint,
                use_tls,
            })
        }
    }
}

/// The grammar a saved direct connection reconnects through. Passwords travel
/// in the target, not the grammar, because socket and pipe grammars have no
/// query string to carry one. TCP and SSH grammars keep the saved password as
/// an encoded query value for parity with pasted links.
pub fn grammar_for_connection(
    connection: &HostConnection,
    password: Option<&str>,
) -> Option<String> {
    let mut grammar = match connection {
        HostConnection::Routed { .. } | HostConnection::Relay { .. } => return None,
        HostConnection::DirectTcp {
            endpoint, use_tls, ..
        } => match use_tls {
            Some(true) => format!("wss://{endpoint}/ws"),
            Some(false) => format!("ws://{endpoint}/ws"),
            None => format!("tcp://{endpoint}"),
        },
        HostConnection::DirectSocket { path, .. } => format!("unix://{path}"),
        HostConnection::DirectPipe { path, .. } => format!("pipe://{path}"),
        HostConnection::RemoteSsh {
            host,
            ssh_port,
            daemon_port,
            ..
        } => {
            let mut grammar = format!("ssh://{host}");
            if let Some(port) = ssh_port {
                grammar.push_str(&format!(":{port}"));
            }
            if let Some(port) = daemon_port {
                grammar.push_str(&format!("?daemonPort={port}"));
            }
            grammar
        }
    };
    let password = password.filter(|password| !password.is_empty());
    if let Some(password) = password
        && matches!(
            connection,
            HostConnection::DirectTcp { .. } | HostConnection::RemoteSsh { .. }
        )
    {
        grammar.push(if grammar.contains('?') { '&' } else { '?' });
        grammar.push_str("password=");
        grammar.push_str(&encode_query_component(password));
    }
    Some(grammar)
}

fn websocket_endpoint(url: &str) -> Option<(String, Option<bool>)> {
    let (scheme, rest) = url.split_once("://")?;
    let use_tls = match scheme {
        "ws" => None,
        "wss" => Some(true),
        _ => return None,
    };
    let (authority, path_and_query) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return None;
    }
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    if !matches!(path, "" | "ws") || !query.is_empty() {
        return None;
    }
    // The WebSocket connector applies the scheme default when the URL omits
    // the port, so the saved record fills it in explicitly: a saved host must
    // replay the endpoint it connected to, not rely on connector defaults.
    let endpoint = match normalize_host_port(authority) {
        Ok(endpoint) => endpoint,
        Err(_) => normalize_host_port(&format!(
            "{authority}:{}",
            if use_tls == Some(true) { 443 } else { 80 }
        ))
        .ok()?,
    };
    let endpoint = normalize_loopback_to_localhost(&endpoint).ok()?;
    Some((endpoint, use_tls))
}

pub fn relay_connection_from_offer(offer: &ConnectionOffer) -> HostConnection {
    // COMPAT(oldRelayOfferTls): added in v0.1.73, remove after 2026-11-10.
    let use_tls = offer
        .relay
        .use_tls
        .unwrap_or_else(|| should_use_tls_for_default_hosted_relay(&offer.relay.endpoint));
    let relay_endpoint =
        normalize_host_port(&offer.relay.endpoint).unwrap_or_else(|_| offer.relay.endpoint.clone());
    HostConnection::Relay {
        id: if use_tls {
            format!("relay:wss:{relay_endpoint}")
        } else {
            format!("relay:{relay_endpoint}")
        },
        relay_endpoint,
        use_tls: Some(use_tls).filter(|_| use_tls),
        daemon_public_key_b64: offer.daemon_public_key_b64.trim().to_owned(),
        minimum_security_mode: offer
            .minimum_security_mode
            .clone()
            .filter(|mode| mode == SECURITY_MODE_PROTECTED),
    }
}

pub fn create_remote_ssh_host_connection(input: SshConnectionInput) -> Result<HostConnection> {
    let host = validate_ssh_host(input.host)?;
    let ssh_port = match input.ssh_port {
        Some(port) => Some(validate_port(port, "SSH port")?),
        None => None,
    };
    let daemon_port = match input.daemon_port {
        Some(port) if port != DEFAULT_SSH_DAEMON_PORT => Some(validate_port(port, "Daemon port")?),
        _ => None,
    };
    let id = [
        "ssh".to_owned(),
        urlencode_component(&host),
        ssh_port.map(|port| port.to_string()).unwrap_or_default(),
        daemon_port.map(|port| port.to_string()).unwrap_or_default(),
    ]
    .join(":");
    Ok(HostConnection::RemoteSsh {
        id,
        host,
        ssh_port,
        daemon_port,
    })
}

pub struct SshConnectionInput<'a> {
    pub host: &'a str,
    pub ssh_port: Option<u16>,
    pub daemon_port: Option<u16>,
}

fn urlencode_component(value: &str) -> String {
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

pub fn order_hosts_local_first<T: HasServerId>(
    hosts: Vec<T>,
    local_server_id: Option<&str>,
) -> Vec<T> {
    let Some(local) = local_server_id else {
        return hosts;
    };
    let mut hosts = hosts;
    let Some(index) = hosts.iter().position(|host| host.server_id() == local) else {
        return hosts;
    };
    if index == 0 {
        return hosts;
    }
    let local_host = hosts.remove(index);
    hosts.insert(0, local_host);
    hosts
}

pub trait HasServerId {
    fn server_id(&self) -> &str;
}

impl HasServerId for String {
    fn server_id(&self) -> &str {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_relay_host_reconstructs_its_full_protected_target() {
        let connection = HostConnection::Relay {
            id: "relay:wss:relay.paseo.sh:443".into(),
            relay_endpoint: "relay.paseo.sh:443".into(),
            use_tls: Some(true),
            daemon_public_key_b64: "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w=".into(),
            minimum_security_mode: Some(SECURITY_MODE_PROTECTED.into()),
        };
        let uri = connection
            .relay_connection_uri("srv_relay", Some("secret password"))
            .expect("relay connection builds a grammar");
        let target = super::super::target::parse_host_target(&uri).unwrap();
        let super::super::target::HostTarget::Socket {
            url,
            password,
            offer: Some(offer),
            description,
            ..
        } = target
        else {
            panic!("expected a relay socket target, got {target:?}");
        };
        assert_eq!(
            url,
            "wss://relay.paseo.sh:443/ws?serverId=srv_relay&role=client&v=2"
        );
        assert_eq!(password.as_deref(), Some("secret password"));
        assert_eq!(offer.server_id, "srv_relay");
        assert_eq!(
            offer.daemon_public_key_b64,
            "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w="
        );
        assert_eq!(
            offer.minimum_security_mode.as_deref(),
            Some(SECURITY_MODE_PROTECTED)
        );
        assert_eq!(description, "relay://relay.paseo.sh:443/srv_relay");
        // A bare route still round-trips without inventing a password floor.
        let bare = HostConnection::Relay {
            id: "relay:relay.paseo.sh:80".into(),
            relay_endpoint: "relay.paseo.sh:80".into(),
            use_tls: None,
            daemon_public_key_b64: "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w=".into(),
            minimum_security_mode: None,
        };
        let uri = bare.relay_connection_uri("srv", None).unwrap();
        assert!(uri.contains("/srv?"));
        assert!(!uri.contains("password="));
    }
    use crate::hosts::target::parse_host_target;

    #[test]
    fn saves_explicit_wss_and_rejects_urls_the_model_cannot_replay() {
        let target = parse_host_target("wss://example.com:6767/ws").unwrap();
        assert_eq!(
            connection_from_target(&target),
            Some(HostConnection::DirectTcp {
                id: "direct:example.com:6767".into(),
                endpoint: "example.com:6767".into(),
                use_tls: Some(true),
            })
        );
        let target = parse_host_target("ws://127.0.0.1:6767/ws").unwrap();
        assert!(matches!(
            connection_from_target(&target),
            Some(HostConnection::DirectTcp {
                endpoint,
                use_tls: None,
                ..
            }) if endpoint == "localhost:6767"
        ));
        for connection in [
            "wss://example.com/custom",
            "wss://example.com/ws?token=secret",
        ] {
            let target = parse_host_target(connection).unwrap();
            assert!(connection_from_target(&target).is_none(), "{connection}");
        }
    }

    #[test]
    fn saves_scheme_default_port_when_the_url_omits_it() {
        let target = parse_host_target("wss://box.example.com/ws").unwrap();
        assert_eq!(
            connection_from_target(&target),
            Some(HostConnection::DirectTcp {
                id: "direct:box.example.com:443".into(),
                endpoint: "box.example.com:443".into(),
                use_tls: Some(true),
            })
        );
        let target = parse_host_target("ws://box.example.com/ws").unwrap();
        assert_eq!(
            connection_from_target(&target),
            Some(HostConnection::DirectTcp {
                id: "direct:box.example.com:80".into(),
                endpoint: "box.example.com:80".into(),
                use_tls: None,
            })
        );
    }

    #[test]
    fn saved_grammar_preserves_tls_and_encodes_special_passwords() {
        let connection = HostConnection::DirectTcp {
            id: "direct:example.com:6767".into(),
            endpoint: "example.com:6767".into(),
            use_tls: Some(true),
        };
        let grammar = grammar_for_connection(&connection, Some("a&b=c#d% e+f")).unwrap();
        assert_eq!(
            grammar,
            "wss://example.com:6767/ws?password=a%26b%3Dc%23d%25%20e%2Bf"
        );
        let parsed = parse_host_target(&grammar).unwrap();
        assert_eq!(parsed.password(), Some("a&b=c#d% e+f"));
        let connection = HostConnection::DirectSocket {
            id: "socket:/tmp/paseo.sock".into(),
            path: "/tmp/paseo.sock".into(),
        };
        assert_eq!(
            grammar_for_connection(&connection, Some("ignored-in-grammar")).unwrap(),
            "unix:///tmp/paseo.sock"
        );
        let connection = HostConnection::RemoteSsh {
            id: "ssh:build-box::".into(),
            host: "build-box".into(),
            ssh_port: None,
            daemon_port: None,
        };
        let grammar = grammar_for_connection(&connection, Some("a&b")).unwrap();
        assert_eq!(grammar, "ssh://build-box?password=a%26b");
        assert_eq!(parse_host_target(&grammar).unwrap().password(), Some("a&b"));
    }
}

//! Saved host profiles mirroring the app's stored registry
//! (`StoredHostRegistrySchema` in `packages/app/src/types/host-connection.ts`).

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::connection::{HostConnection, SECURITY_MODE_PROTECTED, order_hosts_local_first};
use super::target;

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredHostProfile {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<serde_json::Value>,
    pub connections: Vec<StoredHostConnection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StoredHostConnection {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    // directTcp
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_tls: Option<bool>,
    // COMPAT(connectionPassword): added in v0.9.1, remove after 2027-03-24 once
    // stored direct passwords have migrated to the profile-level field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    // directSocket / directPipe
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    // remoteSsh
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_port: Option<u16>,
    // relay
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_public_key_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_security_mode: Option<String>,
    // routed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_server_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostProfile {
    pub server_id: String,
    pub password: Option<String>,
    pub label: String,
    pub appearance: Option<serde_json::Value>,
    pub lifecycle: Option<serde_json::Value>,
    pub connections: Vec<HostConnection>,
    pub preferred_connection_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl HasServerId for HostProfile {
    fn server_id(&self) -> &str {
        &self.server_id
    }
}

pub use super::connection::HasServerId;

pub fn normalize_host_label(value: Option<&str>, server_id: &str) -> String {
    let trimmed = value.unwrap_or_default().trim();
    if trimmed.is_empty() {
        server_id.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Stored records are normalized the way the app does: invalid connections are
/// dropped, an entry without any valid connection is dropped, a legacy
/// per-connection password migrates to the profile field.
pub fn normalize_stored_host_profile(entry: &StoredHostProfile) -> Option<HostProfile> {
    let server_id = entry.server_id.trim();
    if server_id.is_empty() {
        return None;
    }
    let legacy_password = entry.connections.iter().find_map(|connection| {
        if connection.kind == "directTcp" {
            connection
                .password
                .clone()
                .filter(|password| !password.is_empty())
        } else {
            None
        }
    });
    let password = entry
        .password
        .clone()
        .filter(|password| !password.is_empty())
        .or(legacy_password);

    let mut connections = Vec::new();
    for connection in &entry.connections {
        if let Some(connection) = normalize_stored_connection(connection) {
            connections.push(connection);
        }
    }
    if connections.is_empty() {
        return None;
    }

    let created_at = entry
        .created_at
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(now_iso);
    let updated_at = entry
        .updated_at
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| created_at.clone());
    let label = normalize_host_label(entry.label.as_deref(), server_id);
    let preferred_connection_id = entry
        .preferred_connection_id
        .as_deref()
        .filter(|id| connections.iter().any(|connection| connection.id() == *id))
        .map(str::to_owned)
        .or_else(|| {
            connections
                .first()
                .map(|connection| connection.id().to_owned())
        });
    Some(HostProfile {
        server_id: server_id.to_owned(),
        password,
        label,
        appearance: entry.appearance.clone(),
        lifecycle: entry.lifecycle.clone(),
        connections,
        preferred_connection_id,
        created_at,
        updated_at,
    })
}

fn normalize_stored_connection(connection: &StoredHostConnection) -> Option<HostConnection> {
    match connection.kind.as_str() {
        "routed" => {
            let origin_server_id = connection
                .origin_server_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(HostConnection::Routed {
                id: connection
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("routed:{origin_server_id}")),
                origin_server_id: origin_server_id.to_owned(),
            })
        }
        "directTcp" => {
            let raw = connection.endpoint.as_deref()?;
            let endpoint = super::endpoints::normalize_loopback_to_localhost(
                &super::endpoints::normalize_host_port(raw).ok()?,
            )
            .ok()?;
            let use_tls = connection.use_tls;
            Some(HostConnection::DirectTcp {
                id: format!("direct:{endpoint}"),
                endpoint,
                use_tls,
            })
        }
        "directSocket" => {
            let path = connection
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())?;
            Some(HostConnection::DirectSocket {
                id: format!("socket:{path}"),
                path: path.to_owned(),
            })
        }
        "directPipe" => {
            let path = connection
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())?;
            Some(HostConnection::DirectPipe {
                id: format!("pipe:{path}"),
                path: path.to_owned(),
            })
        }
        "remoteSsh" => {
            let host =
                target::validate_ssh_host(connection.host.as_deref().unwrap_or_default()).ok()?;
            let created = super::connection::create_remote_ssh_host_connection(
                super::connection::SshConnectionInput {
                    host: &host,
                    ssh_port: connection.ssh_port,
                    daemon_port: connection.daemon_port,
                },
            )
            .ok()?;
            Some(created)
        }
        "relay" => {
            let relay_endpoint =
                super::endpoints::normalize_host_port(connection.relay_endpoint.as_deref()?)
                    .ok()?;
            let daemon_public_key_b64 = connection
                .daemon_public_key_b64
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())?;
            let use_tls = connection.use_tls;
            let id = if use_tls == Some(true) {
                format!("relay:wss:{relay_endpoint}")
            } else {
                format!("relay:{relay_endpoint}")
            };
            Some(HostConnection::Relay {
                id,
                relay_endpoint,
                use_tls,
                daemon_public_key_b64: daemon_public_key_b64.to_owned(),
                minimum_security_mode: connection
                    .minimum_security_mode
                    .clone()
                    .filter(|mode| mode == SECURITY_MODE_PROTECTED),
            })
        }
        _ => None,
    }
}

/// Records that the relay daemon identity negotiated protected endpoint
/// security. Every relay route to that identity is pinned. Returns the same
/// vector when nothing changes.
pub fn pin_relay_protected_security_in_profiles(
    profiles: Vec<HostProfile>,
    server_id: &str,
    daemon_public_key_b64: &str,
) -> Vec<HostProfile> {
    let now = now_iso();
    let mut changed = false;
    profiles
        .into_iter()
        .map(|mut host| {
            if host.server_id != server_id {
                return host;
            }
            host.connections = host
                .connections
                .into_iter()
                .map(|connection| match connection {
                    HostConnection::Relay {
                        id,
                        relay_endpoint,
                        use_tls,
                        daemon_public_key_b64: key,
                        minimum_security_mode,
                    } if key == daemon_public_key_b64
                        && minimum_security_mode.as_deref() != Some(SECURITY_MODE_PROTECTED) =>
                    {
                        changed = true;
                        HostConnection::Relay {
                            id,
                            relay_endpoint,
                            use_tls,
                            daemon_public_key_b64: key,
                            minimum_security_mode: Some(SECURITY_MODE_PROTECTED.to_owned()),
                        }
                    }
                    other => other,
                })
                .collect();
            if changed {
                host.updated_at = now.clone();
            }
            host
        })
        .collect()
}

/// Adds or merges a connection into the saved profiles, following the app's
/// `upsertHostConnectionInProfiles`: profiles matching by serverId or by an
/// identical connection identity merge into one profile whose earliest
/// createdAt, customized label and pin survive.
pub fn upsert_host_connection_in_profiles(
    profiles: Vec<HostProfile>,
    input: UpsertInput,
) -> Result<Vec<HostProfile>> {
    let server_id = input.server_id.trim();
    if server_id.is_empty() {
        anyhow::bail!("serverId is required");
    }
    let now = now_iso();
    let derived_label = normalize_host_label(input.label.as_deref(), server_id);
    let matching: Vec<usize> = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            profile.server_id == server_id
                || (!matches!(input.connection, HostConnection::Routed { .. })
                    && profile
                        .connections
                        .iter()
                        .any(|existing| existing.equals_identity(&input.connection)))
        })
        .map(|(index, _)| index)
        .collect();

    if matching.is_empty() {
        let password = input
            .password
            .clone()
            .filter(|password| !password.is_empty());
        let profile = HostProfile {
            server_id: server_id.to_owned(),
            password,
            label: derived_label,
            appearance: None,
            lifecycle: None,
            connections: vec![input.connection.clone()],
            preferred_connection_id: Some(input.connection.id().to_owned()),
            created_at: now.clone(),
            updated_at: now,
        };
        let mut profiles = profiles;
        profiles.push(profile);
        return Ok(profiles);
    }

    let prev_index = matching
        .iter()
        .copied()
        .find(|index| profiles[*index].server_id == server_id)
        .unwrap_or(matching[0]);
    let prev = &profiles[prev_index];

    let mut merged_connections: Vec<HostConnection> = Vec::new();
    for profile_index in &matching {
        for connection in &profiles[*profile_index].connections {
            merged_connections.push(connection.clone());
        }
    }
    let mut pin_sources = merged_connections.clone();
    pin_sources.push(input.connection.clone());
    let mut merged_connections = std::mem::take(&mut merged_connections);
    // Replace the record with the same id, then make sure the incoming
    // connection is present exactly once.
    merged_connections.retain(|existing| existing.id() != input.connection.id());
    merged_connections.push(input.connection.clone());
    let next_connections = share_relay_security_pin_with_sources(merged_connections, &pin_sources);

    let next_label = if prev.label == prev.server_id {
        derived_label
    } else {
        prev.label.clone()
    };
    let next_preferred = prev
        .preferred_connection_id
        .as_deref()
        .filter(|id| {
            next_connections
                .iter()
                .any(|connection| connection.id() == *id)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| input.connection.id().to_owned());
    let next_created_at = matching
        .iter()
        .map(|index| profiles[*index].created_at.as_str())
        .min()
        .unwrap_or(&prev.created_at)
        .to_owned();
    let next_password = match &input.password {
        Some(password) if !password.is_empty() => Some(password.clone()),
        _ => prev.password.clone(),
    };

    let mut next = Vec::with_capacity(profiles.len());
    for (index, mut profile) in profiles.into_iter().enumerate() {
        if index == prev_index {
            profile.server_id = server_id.to_owned();
            profile.password = next_password.clone();
            profile.label = next_label.clone();
            profile.connections = next_connections.clone();
            profile.preferred_connection_id = Some(next_preferred.clone());
            profile.created_at = next_created_at.clone();
            profile.updated_at = now.clone();
        } else if matching.contains(&index) {
            // Merged into the surviving profile.
            continue;
        }
        next.push(profile);
    }
    Ok(next)
}

pub struct UpsertInput {
    pub server_id: String,
    pub label: Option<String>,
    pub connection: HostConnection,
    pub password: Option<String>,
}

/// The security pin belongs to the daemon identity, not to one relay address.
/// Once any relay route to a key is pinned, every route to that key is pinned.
pub fn share_relay_security_pin(connections: Vec<HostConnection>) -> Vec<HostConnection> {
    let sources = connections.clone();
    share_relay_security_pin_with_sources(connections, &sources)
}

fn share_relay_security_pin_with_sources(
    connections: Vec<HostConnection>,
    pin_sources: &[HostConnection],
) -> Vec<HostConnection> {
    let pinned_keys: Vec<String> = pin_sources
        .iter()
        .filter_map(|connection| match connection {
            HostConnection::Relay {
                daemon_public_key_b64,
                minimum_security_mode: Some(mode),
                ..
            } if mode == SECURITY_MODE_PROTECTED => Some(daemon_public_key_b64.clone()),
            _ => None,
        })
        .collect();
    connections
        .into_iter()
        .map(|connection| match connection {
            HostConnection::Relay {
                id,
                relay_endpoint,
                use_tls,
                daemon_public_key_b64,
                minimum_security_mode,
            } if minimum_security_mode.as_deref() != Some(SECURITY_MODE_PROTECTED)
                && pinned_keys.contains(&daemon_public_key_b64) =>
            {
                HostConnection::Relay {
                    id,
                    relay_endpoint,
                    use_tls,
                    daemon_public_key_b64,
                    minimum_security_mode: Some(SECURITY_MODE_PROTECTED.to_owned()),
                }
            }
            other => other,
        })
        .collect()
}

pub fn serialize_registry(profiles: &[HostProfile]) -> Result<Vec<u8>> {
    let stored: Vec<StoredHostProfile> = profiles
        .iter()
        .map(|profile| StoredHostProfile {
            server_id: profile.server_id.clone(),
            password: profile.password.clone(),
            label: Some(profile.label.clone()),
            appearance: profile.appearance.clone(),
            lifecycle: profile.lifecycle.clone(),
            connections: profile
                .connections
                .iter()
                .map(|connection| stored_connection_from(connection))
                .collect(),
            preferred_connection_id: profile.preferred_connection_id.clone(),
            created_at: Some(profile.created_at.clone()),
            updated_at: Some(profile.updated_at.clone()),
        })
        .collect();
    let mut bytes = serde_json::to_vec_pretty(&stored)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn stored_connection_from(connection: &HostConnection) -> StoredHostConnection {
    match connection {
        HostConnection::Routed {
            id,
            origin_server_id,
        } => StoredHostConnection {
            kind: "routed".into(),
            id: Some(id.clone()),
            origin_server_id: Some(origin_server_id.clone()),
            endpoint: None,
            use_tls: None,
            password: None,
            path: None,
            host: None,
            ssh_port: None,
            daemon_port: None,
            relay_endpoint: None,
            daemon_public_key_b64: None,
            minimum_security_mode: None,
        },
        HostConnection::DirectTcp {
            id,
            endpoint,
            use_tls,
        } => StoredHostConnection {
            kind: "directTcp".into(),
            id: Some(id.clone()),
            endpoint: Some(endpoint.clone()),
            use_tls: *use_tls,
            origin_server_id: None,
            password: None,
            path: None,
            host: None,
            ssh_port: None,
            daemon_port: None,
            relay_endpoint: None,
            daemon_public_key_b64: None,
            minimum_security_mode: None,
        },
        HostConnection::DirectSocket { id, path } => StoredHostConnection {
            kind: "directSocket".into(),
            id: Some(id.clone()),
            path: Some(path.clone()),
            origin_server_id: None,
            endpoint: None,
            use_tls: None,
            password: None,
            host: None,
            ssh_port: None,
            daemon_port: None,
            relay_endpoint: None,
            daemon_public_key_b64: None,
            minimum_security_mode: None,
        },
        HostConnection::DirectPipe { id, path } => StoredHostConnection {
            kind: "directPipe".into(),
            id: Some(id.clone()),
            path: Some(path.clone()),
            origin_server_id: None,
            endpoint: None,
            use_tls: None,
            password: None,
            host: None,
            ssh_port: None,
            daemon_port: None,
            relay_endpoint: None,
            daemon_public_key_b64: None,
            minimum_security_mode: None,
        },
        HostConnection::RemoteSsh {
            id,
            host,
            ssh_port,
            daemon_port,
        } => StoredHostConnection {
            kind: "remoteSsh".into(),
            id: Some(id.clone()),
            host: Some(host.clone()),
            ssh_port: *ssh_port,
            daemon_port: *daemon_port,
            origin_server_id: None,
            endpoint: None,
            use_tls: None,
            password: None,
            path: None,
            relay_endpoint: None,
            daemon_public_key_b64: None,
            minimum_security_mode: None,
        },
        HostConnection::Relay {
            id,
            relay_endpoint,
            use_tls,
            daemon_public_key_b64,
            minimum_security_mode,
        } => StoredHostConnection {
            kind: "relay".into(),
            id: Some(id.clone()),
            relay_endpoint: Some(relay_endpoint.clone()),
            use_tls: *use_tls,
            daemon_public_key_b64: Some(daemon_public_key_b64.clone()),
            minimum_security_mode: minimum_security_mode.clone(),
            origin_server_id: None,
            endpoint: None,
            password: None,
            path: None,
            host: None,
            ssh_port: None,
            daemon_port: None,
        },
    }
}

pub fn order_profiles_local_first(
    profiles: Vec<HostProfile>,
    local_server_id: Option<&str>,
) -> Vec<HostProfile> {
    order_hosts_local_first(profiles, local_server_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(endpoint: &str) -> HostConnection {
        HostConnection::DirectTcp {
            id: format!("direct:{endpoint}"),
            endpoint: endpoint.to_owned(),
            use_tls: None,
        }
    }

    fn profile(server_id: &str, connection: HostConnection) -> HostProfile {
        HostProfile {
            server_id: server_id.to_owned(),
            password: None,
            label: server_id.to_owned(),
            appearance: None,
            lifecycle: None,
            connections: vec![connection],
            preferred_connection_id: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn drops_invalid_entries_and_migrates_legacy_direct_passwords() {
        let bytes = r#"[
            {"serverId":"srv_ok","connections":[{"type":"directTcp","endpoint":"127.0.0.1:6767","password":"secret"}]},
            {"serverId":"srv_bad","connections":[{"type":"unknownKind"}]},
            {"serverId":"srv_empty","connections":[]},
            {"connections":[{"type":"directSocket","path":"/tmp/s"}]}
        ]"#;
        // Registry parsing is per-entry: one invalid entry never discards the rest.
        let normalized = super::super::registry::parse_registry(bytes.as_bytes());
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].server_id, "srv_ok");
        assert_eq!(normalized[0].password.as_deref(), Some("secret"));
        assert_eq!(normalized[0].connections[0], tcp("localhost:6767"));
    }

    #[test]
    fn rejects_stored_entries_with_unknown_fields() {
        let result: std::result::Result<Vec<StoredHostProfile>, _> = serde_json::from_str(
            r#"[{"serverId":"srv","extra":"field","connections":[{"type":"directSocket","path":"/tmp/s"}]}]"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn upsert_merges_profiles_that_share_a_connection_identity() {
        let existing = vec![profile("srv_a", tcp("localhost:6767"))];
        let ssh = super::super::connection::create_remote_ssh_host_connection(
            super::super::connection::SshConnectionInput {
                host: "build-box",
                ssh_port: None,
                daemon_port: None,
            },
        )
        .unwrap();
        let merged = upsert_host_connection_in_profiles(
            existing,
            UpsertInput {
                server_id: "srv_a".into(),
                label: None,
                connection: ssh.clone(),
                password: None,
            },
        )
        .unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].connections.len(), 2);
        assert_eq!(merged[0].created_at, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_relay_pin_spreads_across_routes_to_the_same_identity() {
        let connections = vec![
            HostConnection::Relay {
                id: "relay:r:443".into(),
                relay_endpoint: "relay.paseo.sh:443".into(),
                use_tls: Some(true),
                daemon_public_key_b64: "key-a".into(),
                minimum_security_mode: Some(SECURITY_MODE_PROTECTED.into()),
            },
            HostConnection::Relay {
                id: "relay:alt:443".into(),
                relay_endpoint: "alt.relay:443".into(),
                use_tls: Some(true),
                daemon_public_key_b64: "key-a".into(),
                minimum_security_mode: None,
            },
            HostConnection::Relay {
                id: "relay:other:443".into(),
                relay_endpoint: "other:443".into(),
                use_tls: Some(true),
                daemon_public_key_b64: "key-b".into(),
                minimum_security_mode: None,
            },
        ];
        let pinned = share_relay_security_pin(connections);
        fn mode(connection: &HostConnection) -> Option<&str> {
            match connection {
                HostConnection::Relay {
                    minimum_security_mode,
                    ..
                } => minimum_security_mode.as_deref(),
                _ => None,
            }
        }
        assert_eq!(mode(&pinned[1]), Some(SECURITY_MODE_PROTECTED));
        assert_eq!(mode(&pinned[2]), None);
    }

    #[test]
    fn replacing_a_pinned_relay_record_keeps_the_pin() {
        let pinned = HostConnection::Relay {
            id: "relay:r:443".into(),
            relay_endpoint: "r.example:443".into(),
            use_tls: Some(true),
            daemon_public_key_b64: "key-a".into(),
            minimum_security_mode: Some(SECURITY_MODE_PROTECTED.into()),
        };
        let unpinned = HostConnection::Relay {
            id: "relay:r:443".into(),
            relay_endpoint: "r.example:443".into(),
            use_tls: Some(true),
            daemon_public_key_b64: "key-a".into(),
            minimum_security_mode: None,
        };
        let next = upsert_host_connection_in_profiles(
            vec![profile("srv_a", pinned)],
            UpsertInput {
                server_id: "srv_a".into(),
                label: None,
                connection: unpinned,
                password: None,
            },
        )
        .unwrap();
        assert!(matches!(
            &next[0].connections[0],
            HostConnection::Relay { minimum_security_mode: Some(mode), .. }
                if mode == SECURITY_MODE_PROTECTED
        ));
    }

    #[test]
    fn online_adopt_pins_every_relay_route_to_the_authenticated_daemon_key() {
        let relay = |endpoint: &str, key: &str, pinned: bool| HostConnection::Relay {
            // Canonical id shape the stored grammar derives for a TLS route.
            id: format!("relay:wss:{endpoint}"),
            relay_endpoint: endpoint.into(),
            use_tls: Some(true),
            daemon_public_key_b64: key.into(),
            minimum_security_mode: pinned.then(|| SECURITY_MODE_PROTECTED.to_owned()),
        };
        let mut target_host = profile("srv_a", relay("relay.paseo.sh:443", "key-a", false));
        target_host
            .connections
            .push(relay("alt.relay:443", "key-a", false));
        target_host
            .connections
            .push(relay("other:443", "key-b", false));
        target_host.preferred_connection_id = Some("relay:wss:relay.paseo.sh:443".into());
        let mut other_host = profile("srv_b", relay("relay.paseo.sh:443", "key-a", false));
        other_host.preferred_connection_id = Some("relay:wss:relay.paseo.sh:443".into());
        let profiles = vec![target_host.clone(), other_host.clone()];

        let pinned = pin_relay_protected_security_in_profiles(profiles.clone(), "srv_a", "key-a");
        // Every relay route to the authenticated key on the adopted host pins;
        // routes to other keys and other hosts stay untouched.
        let modes: Vec<Option<&str>> = pinned[0]
            .connections
            .iter()
            .map(|connection| match connection {
                HostConnection::Relay {
                    minimum_security_mode,
                    ..
                } => minimum_security_mode.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(
            modes,
            vec![
                Some(SECURITY_MODE_PROTECTED),
                Some(SECURITY_MODE_PROTECTED),
                None
            ]
        );
        assert_eq!(pinned[1], other_host);
        assert_ne!(pinned[0], target_host);
        // The pin survives the registry round trip.
        let bytes = serialize_registry(&pinned).unwrap();
        let restored: Vec<_> = serde_json::from_slice::<Vec<StoredHostProfile>>(&bytes)
            .unwrap()
            .iter()
            .filter_map(normalize_stored_host_profile)
            .collect();
        assert_eq!(restored, pinned);
        // Idempotent: no change means no persistence trigger upstream.
        assert_eq!(
            pin_relay_protected_security_in_profiles(pinned.clone(), "srv_a", "key-a"),
            pinned
        );
    }

    #[test]
    fn registry_round_trips_through_serialization() {
        let mut stored = profile("srv_a", tcp("localhost:6767"));
        stored.preferred_connection_id = Some("direct:localhost:6767".into());
        let profiles = vec![stored];
        let bytes = serialize_registry(&profiles).unwrap();
        let stored: Vec<StoredHostProfile> = serde_json::from_slice(&bytes).unwrap();
        let restored: Vec<_> = stored
            .iter()
            .filter_map(normalize_stored_host_profile)
            .collect();
        assert_eq!(restored, profiles);
    }
}

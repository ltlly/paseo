//! Host connection runtime: resolves a [`HostTarget`] into a real daemon
//! connection, verifies the pinned identity, and reports honest
//! pending/online/failed states. One active connection at a time; closing a
//! connection never touches the remote daemon's terminals.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};

use crate::client::{Connection, Event, Target};
use crate::model::ServerInfo;

use super::ssh::SshTunnel;
use super::target::HostTarget;

/// A host that was discovered or saved under one identity must never be
/// silently re-pointed at a different daemon. Identity is pinned when learned.
#[derive(Debug)]
pub struct IdentityMismatch {
    pub expected: Option<String>,
    pub learned: String,
}

impl std::fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.expected.as_deref() {
            Some(expected) => write!(
                f,
                "This address now serves host {learned}, not the saved host {expected}. Remove the saved host or replace its identity explicitly.",
                learned = self.learned
            ),
            None => write!(f, "Host identity changed to {}.", self.learned),
        }
    }
}

pub enum HostRuntimeEvent {
    /// `Ready` accepted: the learned identity matches the pinned one.
    Connected(ServerInfo),
    ConnectedWithoutIdentity(ServerInfo),
    IdentityMismatch(IdentityMismatch),
    /// Session or terminal traffic from the active connection.
    Event(Event),
    Disconnected(String),
}

/// One live host connection attempt with its transport resources.
pub struct HostClient {
    pub connection: Arc<Connection>,
    pub events: Receiver<HostRuntimeEvent>,
    /// The expected identity this client was started with, if known.
    pub expected_server_id: Option<String>,
    tunnel: Arc<tokio::sync::Mutex<Option<Arc<SshTunnel>>>>,
}

impl HostClient {
    /// Starts a client over an already-resolved target, e.g. the local daemon
    /// discovered from `paseo.pid` at startup.
    pub fn start_with_target(target: Target, expected_server_id: Option<String>) -> Result<Self> {
        let expected = expected_server_id.clone();
        let connection = Connection::start(target).context("start the host connection")?;
        Self::assemble(connection, expected, None)
    }

    fn assemble(
        connection: Connection,
        expected_server_id: Option<String>,
        tunnel: Option<Arc<SshTunnel>>,
    ) -> Result<Self> {
        let connection = Arc::new(connection);
        let (events, event_receiver) = async_channel::bounded::<HostRuntimeEvent>(8);
        let network_events = connection.events.clone();
        let forward_connection = connection.clone();
        let forward_expected = expected_server_id.clone();
        std::thread::Builder::new()
            .name("paseo-host-runtime".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("start the host runtime");
                let result = runtime.and_then(|runtime| {
                    runtime.block_on(async {
                        forward_events(
                            &network_events,
                            &events,
                            forward_expected,
                            forward_connection,
                        )
                        .await
                    })
                });
                if let Err(error) = result {
                    let _ =
                        events.send_blocking(HostRuntimeEvent::Disconnected(format!("{error:#}")));
                }
            })
            .context("start the host runtime thread")?;
        Ok(Self {
            connection,
            events: event_receiver,
            expected_server_id,
            tunnel: Arc::new(tokio::sync::Mutex::new(tunnel)),
        })
    }

    /// Starts a real connection. The tunnel step is synchronous because SSH
    /// process ownership stays explicit; the network loop is the shared
    /// client connection lifecycle.
    pub fn start(input: ConnectInput) -> Result<Self> {
        let expected_server_id = pinned_server_id(&input);
        let (target, tunnel) = resolve(&input.target)?;
        let connection = Connection::start(target).context("start the host connection")?;
        Self::assemble(connection, expected_server_id, tunnel)
    }

    pub fn close(&self) {
        self.connection.close();
        // The SSH child must not outlive the client. Kill it from a detached
        // thread so the UI thread never blocks on process teardown.
        let tunnel = self.tunnel.clone();
        std::thread::Builder::new()
            .name("paseo-host-tunnel-close".into())
            .spawn(move || {
                let tunnel = tunnel.try_lock();
                if let Ok(tunnel) = tunnel
                    && let Some(tunnel) = tunnel.as_ref()
                {
                    tunnel.close();
                }
            })
            .ok();
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone)]
pub struct ConnectInput {
    pub target: HostTarget,
    /// The identity a saved profile pins. `None` learns the identity.
    pub expected_server_id: Option<String>,
}

/// The identity a connection pins: the saved profile's, or for relay hosts
/// without one, the offer's `serverId` — a relay host must never silently
/// re-point at a different daemon.
fn pinned_server_id(input: &ConnectInput) -> Option<String> {
    input
        .expected_server_id
        .clone()
        .or_else(|| match &input.target {
            crate::hosts::target::HostTarget::Socket {
                offer: Some(offer), ..
            } => Some(offer.server_id.clone()),
            _ => None,
        })
}

/// Resolves a grammar target into the concrete websocket target the shared
/// client dials. Socket and pipe paths use the client's stream transport;
/// relay targets still need the E2EE secure channel, so they report an
/// explicit gap instead of pretending to connect.
fn resolve(target: &HostTarget) -> Result<(Target, Option<Arc<SshTunnel>>)> {
    match target {
        HostTarget::Socket {
            url,
            socket_path,
            pipe,
            password,
            offer,
            ..
        } => {
            if let Some(offer) = offer {
                // Relay offers resolve into the E2EE-protected relay target:
                // the daemon public key from the offer authenticates the Noise
                // handshake, and credentials only travel inside it.
                return Ok((
                    Target::relay(
                        url.clone(),
                        offer.daemon_public_key_b64.clone(),
                        password.as_ref().map(|value| value.to_owned()),
                    )?,
                    None,
                ));
            }
            if let Some(path) = socket_path {
                return Ok((
                    Target::socket(std::path::PathBuf::from(path), *pipe, password.clone())?,
                    None,
                ));
            }
            Ok((
                Target::remote(url.clone(), password.as_ref().map(|value| value.to_owned()))?,
                None,
            ))
        }
        HostTarget::Ssh { ssh, password, .. } => {
            let tunnel = Arc::new(super::ssh::open(ssh).context("open the SSH tunnel")?);
            let url = format!("ws://{}/ws", tunnel.endpoint());
            // The tunnel listens on this machine's loopback, but the daemon runs
            // on the SSH host. Direct service URLs name that host, and Paseo's
            // proxy URLs stay withheld because they only resolve daemon-side.
            Ok((
                Target::tunneled(url, ssh.host.clone(), password.clone())?,
                Some(tunnel),
            ))
        }
    }
}

async fn forward_events(
    source: &Receiver<Event>,
    sink: &Sender<HostRuntimeEvent>,
    expected_server_id: Option<String>,
    connection: Arc<Connection>,
) -> Result<()> {
    let mut ready = false;
    while let Ok(event) = source.recv().await {
        match event {
            Event::Ready(info) => {
                if let Some(expected) = &expected_server_id
                    && expected != &info.server_id
                {
                    connection.close();
                    sink.send(HostRuntimeEvent::IdentityMismatch(IdentityMismatch {
                        expected: Some(expected.clone()),
                        learned: info.server_id.clone(),
                    }))
                    .await
                    .context("host runtime event channel closed")?;
                    continue;
                }
                ready = true;
                let event = if expected_server_id.is_some() {
                    HostRuntimeEvent::Connected(info)
                } else {
                    HostRuntimeEvent::ConnectedWithoutIdentity(info)
                };
                sink.send(event)
                    .await
                    .context("host runtime event channel closed")?;
            }
            Event::Disconnected(error) => {
                ready = false;
                sink.send(HostRuntimeEvent::Disconnected(error))
                    .await
                    .context("host runtime event channel closed")?;
            }
            other => {
                // Session traffic only means something once the host is ready.
                if ready {
                    sink.send(HostRuntimeEvent::Event(other))
                        .await
                        .context("host runtime event channel closed")?;
                }
            }
        }
    }
    Ok(())
}

/// The honest state of the active host surface, driven only by runtime events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostPhase {
    Idle,
    Connecting,
    Online,
    Failed(String),
}

/// Daemon-side saved registrations (`paseo.hosts`), observed over the active
/// connection. The daemon owns these entries; the native list above is the
/// client's own.
pub mod daemon_registrations {
    use crate::client::Connection;
    use anyhow::{Context as _, Result};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct HostSummary {
        pub registration_id: String,
        pub server_id: Option<String>,
        pub label: Option<String>,
        pub enabled: bool,
        pub status: String,
        pub target_description: String,
        pub last_online_at: Option<String>,
        pub next_retry_at: Option<String>,
        #[serde(default)]
        pub error: Option<Value>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct HostDirectory {
        #[serde(rename = "self")]
        pub self_host: SelfHost,
        pub hosts: Vec<HostSummary>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SelfHost {
        pub server_id: String,
        pub label: String,
    }

    /// `hosts.list.response` payload or a `hosts.changed` push.
    pub fn parse_directory(payload: &Value) -> Result<HostDirectory> {
        let directory = payload
            .get("directory")
            .context("host directory payload is missing")?;
        serde_json::from_value(directory.clone()).context("invalid host directory payload")
    }

    pub fn parse_summary(payload: &Value) -> Result<HostSummary> {
        let host = payload.get("host").context("host payload is missing")?;
        serde_json::from_value(host.clone()).context("invalid host payload")
    }

    pub fn observe_request(connection: &Connection) -> Result<String> {
        connection.subscribe("hosts", "hosts.list.request", json!({"observe": true}))
    }

    pub fn add_request(
        connection: &Connection,
        connection_grammar: &str,
        label: Option<&str>,
    ) -> Result<String> {
        let mut input = json!({"connection": connection_grammar});
        if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
            input["label"] = json!(label.trim());
        }
        connection.request("hosts.add.request", json!({"input": input}))
    }

    pub fn remove_request(connection: &Connection, registration_id: &str) -> Result<String> {
        connection.request(
            "hosts.remove.request",
            json!({"registrationId": registration_id}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServerInfo;
    use futures_util::{SinkExt as _, StreamExt as _};
    use serde_json::{Value, json};

    fn ready_event(server_id: &str) -> Event {
        Event::Ready(ServerInfo {
            server_id: server_id.into(),
            hostname: None,
            version: None,
            features: Default::default(),
        })
    }

    fn daemon_message(kind: &str, payload: Value) -> Event {
        let _ = kind;
        Event::Session {
            kind: kind.into(),
            payload,
        }
    }

    #[tokio::test]
    async fn a_saved_identity_pin_rejects_a_changed_destination() {
        let (source, source_receiver) = async_channel::unbounded();
        let (sink, sink_receiver) = async_channel::unbounded();
        // A stand-in connection is unused in this test; events come from the channel.
        let forward_connection = Arc::new(
            Connection::start(Target::remote("ws://127.0.0.1:1/ws".into(), None).unwrap()).unwrap(),
        );
        let task = tokio::spawn(async move {
            let _ = forward_events(
                &source_receiver,
                &sink,
                Some("srv_expected".into()),
                forward_connection,
            )
            .await;
        });
        source.send(ready_event("srv_other")).await.unwrap();
        match sink_receiver.recv().await.unwrap() {
            HostRuntimeEvent::IdentityMismatch(mismatch) => {
                assert_eq!(mismatch.expected.as_deref(), Some("srv_expected"));
                assert_eq!(mismatch.learned, "srv_other");
                assert!(mismatch.to_string().contains("srv_other"));
            }
            other => panic!(
                "expected identity mismatch, got {}",
                match &other {
                    HostRuntimeEvent::Connected(_) => "Connected",
                    HostRuntimeEvent::ConnectedWithoutIdentity(_) => "ConnectedWithoutIdentity",
                    HostRuntimeEvent::IdentityMismatch(_) => "IdentityMismatch",
                    HostRuntimeEvent::Event(_) => "Event",
                    HostRuntimeEvent::Disconnected(error) => error,
                }
            ),
        }
        task.abort();
    }

    #[tokio::test]
    async fn session_traffic_is_suppressed_until_the_host_is_ready() {
        let (source, source_receiver) = async_channel::unbounded();
        let (sink, sink_receiver) = async_channel::unbounded();
        let forward_connection = Arc::new(
            Connection::start(Target::remote("ws://127.0.0.1:1/ws".into(), None).unwrap()).unwrap(),
        );
        let task = tokio::spawn(async move {
            let _ = forward_events(&source_receiver, &sink, None, forward_connection).await;
        });
        source
            .send(daemon_message("agent_status", json!({})))
            .await
            .unwrap();
        assert!(sink_receiver.try_recv().is_err());
        source.send(ready_event("srv")).await.unwrap();
        assert!(matches!(
            sink_receiver.recv().await.unwrap(),
            HostRuntimeEvent::ConnectedWithoutIdentity(_)
        ));
        source
            .send(daemon_message("agent_status", json!({})))
            .await
            .unwrap();
        assert!(matches!(
            sink_receiver.recv().await.unwrap(),
            HostRuntimeEvent::Event(_)
        ));
        task.abort();
    }

    #[test]
    fn socket_and_pipe_targets_resolve_to_the_stream_transport() {
        for connection in ["unix:///tmp/paseo.sock", "pipe://\\\\.\\pipe\\paseo"] {
            let target = crate::hosts::target::parse_host_target(connection).unwrap();
            let (resolved, tunnel) = resolve(&target).unwrap();
            assert!(tunnel.is_none());
            assert!(resolved.is_stream(), "{connection}");
            assert_eq!(resolved.url, "ws://localhost/ws");
        }
    }

    #[test]
    fn tcp_targets_resolve_without_a_tunnel() {
        let target = crate::hosts::target::parse_host_target("tcp://127.0.0.1:1").unwrap();
        let Ok((resolved, tunnel)) = resolve(&target) else {
            panic!("tcp target must resolve");
        };
        assert!(tunnel.is_none());
        assert!(!resolved.is_stream());
        assert_eq!(resolved.url, "ws://127.0.0.1:1/ws");
    }

    #[test]
    fn relay_offers_resolve_to_the_protected_relay_target_instead_of_the_gap() {
        use crate::hosts::offer::{ConnectionOffer, OfferRelay};
        let offer = ConnectionOffer {
            v: 2,
            server_id: "srv_relay".into(),
            daemon_public_key_b64: "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w=".into(),
            relay: OfferRelay {
                endpoint: "relay.paseo.sh:443".into(),
                use_tls: Some(true),
            },
            minimum_security_mode: Some("protected".into()),
        };
        let target = crate::hosts::target::HostTarget::Socket {
            url: "wss://relay.paseo.sh:443/ws?serverId=srv_relay&role=client&v=2".into(),
            socket_path: None,
            pipe: false,
            password: Some("secret".into()),
            offer: Some(offer),
            description: "relay://relay.paseo.sh:443/srv_relay".into(),
        };
        let Ok((resolved, tunnel)) = resolve(&target) else {
            panic!("relay offer must resolve");
        };
        assert!(tunnel.is_none());
        assert_eq!(
            resolved.url,
            "wss://relay.paseo.sh:443/ws?serverId=srv_relay&role=client&v=2"
        );
        let relay = resolved.relay.expect("relay identity must ride the target");
        assert_eq!(
            relay.daemon_public_key_b64,
            "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w="
        );
        // The password stays in the target's daemon hello auth, never on the
        // relay upgrade URL.
        assert!(!resolved.url.contains("password"));
    }

    #[test]
    fn relay_offer_pins_its_server_id_when_no_identity_is_saved() {
        use crate::hosts::offer::{ConnectionOffer, OfferRelay};
        use crate::hosts::target::HostTarget;
        let input = ConnectInput {
            target: HostTarget::Socket {
                url: "wss://relay.paseo.sh:443/ws?serverId=srv_relay&role=client&v=2".into(),
                socket_path: None,
                pipe: false,
                password: None,
                offer: Some(ConnectionOffer {
                    v: 2,
                    server_id: "srv_relay".into(),
                    daemon_public_key_b64: "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w=".into(),
                    relay: OfferRelay {
                        endpoint: "relay.paseo.sh:443".into(),
                        use_tls: Some(true),
                    },
                    minimum_security_mode: None,
                }),
                description: "relay".into(),
            },
            expected_server_id: None,
        };
        // start() would dial; only verify the pinning seam without connecting.
        assert_eq!(pinned_server_id(&input).as_deref(), Some("srv_relay"));
        // A saved identity always wins over the offer's.
        let mut saved = ConnectInput {
            target: input.target,
            expected_server_id: Some("srv_saved".into()),
        };
        assert_eq!(pinned_server_id(&saved).as_deref(), Some("srv_saved"));
        saved.expected_server_id = None;
        let _ = saved;
    }

    #[cfg(unix)]
    #[test]
    fn host_client_start_keeps_the_ssh_tunnel_alive_through_a_real_dial() {
        use std::os::unix::fs::PermissionsExt as _;

        struct ProgramGuard;
        impl Drop for ProgramGuard {
            fn drop(&mut self) {
                super::super::ssh::set_test_program(None);
            }
        }

        let directory = std::env::temp_dir().join(format!(
            "paseo-ssh-production-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx
                    .send(listener.local_addr().unwrap().port())
                    .unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                assert!(socket.next().await.unwrap().unwrap().to_text().unwrap().contains("hello"));
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"ssh-fixture","features":{}}}})
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                while let Some(Ok(_)) = socket.next().await {}
            });
        });
        let port = port_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let stub = directory.join("ssh");
        std::fs::write(
            &stub,
            format!("#!/bin/bash\nexec 3<>/dev/tcp/127.0.0.1/{port}\ncat <&3 &\ncat >&3\n"),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::super::ssh::set_test_program(Some((
            stub.to_string_lossy().into_owned(),
            Some(directory.to_string_lossy().into_owned()),
        )));
        let _guard = ProgramGuard;

        let target = crate::hosts::target::parse_host_target("ssh://stub-host").unwrap();
        let client = match HostClient::start(ConnectInput {
            target,
            expected_server_id: None,
        }) {
            Ok(client) => client,
            Err(error) => panic!("host client start failed: {error:#}"),
        };
        let events = client.events.clone();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = event_tx.send(events.recv_blocking());
        });
        let event = event_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the SSH tunnel did not deliver a host event");
        assert!(matches!(
            event,
            Ok(HostRuntimeEvent::ConnectedWithoutIdentity(info)) if info.server_id == "ssh-fixture"
        ));
        client.close();
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires an isolated real daemon; set PASEO_NATIVE_SOCKET_SMOKE_HOME"]
    fn local_socket_discovery_connects_an_isolated_real_daemon() {
        let home = std::env::var("PASEO_NATIVE_SOCKET_SMOKE_HOME")
            .expect("set PASEO_NATIVE_SOCKET_SMOKE_HOME to an isolated daemon home");
        let target = Target::local(std::path::Path::new(&home)).unwrap();
        assert!(target.is_local_discovery());
        assert!(target.is_stream());
        let client = HostClient::start_with_target(target, None).unwrap();
        let events = client.events.clone();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = event_tx.send(events.recv_blocking());
        });
        let event = event_rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the real daemon socket did not answer");
        assert!(matches!(
            event,
            Ok(HostRuntimeEvent::ConnectedWithoutIdentity(info)) if !info.server_id.is_empty()
        ));
        client.close();
    }

    #[test]
    fn daemon_directory_payloads_parse() {
        let payload = json!({
            "requestId": "r1",
            "directory": {
                "self": {"serverId": "srv_a", "label": "self"},
                "hosts": [{
                    "registrationId": "reg1",
                    "serverId": "srv_b",
                    "label": "Build box",
                    "source": {"id": "p", "key": "k"},
                    "metadata": {},
                    "enabled": true,
                    "status": "online",
                    "targetDescription": "tcp://10.0.0.2:6767",
                    "lastOnlineAt": "2026-01-01T00:00:00.000Z",
                    "nextRetryAt": null,
                    "error": null
                }]
            }
        });
        let directory = daemon_registrations::parse_directory(&payload).unwrap();
        assert_eq!(directory.self_host.server_id, "srv_a");
        assert_eq!(directory.hosts[0].status, "online");
    }
    #[cfg(unix)]
    #[test]
    fn ssh_reconnect_spawns_a_fresh_tunnel_child_and_keeps_the_first_identity() {
        use std::io::BufRead as _;

        let _dial_lock = super::super::ssh::test_dial_lock();

        struct ProgramGuard;
        impl Drop for ProgramGuard {
            fn drop(&mut self) {
                super::super::ssh::set_test_program(None);
            }
        }

        let directory = std::path::PathBuf::from("/tmp").join(format!(
            "paseo-ssh-reconnect-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        // The fixture daemon serves two sequential WebSocket connections with
        // the same identity: the first session is closed so the supervising
        // client must reconnect through the same tunnel endpoint.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx
                    .send(listener.local_addr().unwrap().port())
                    .unwrap();
                for last in [false, true] {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    assert!(socket
                        .next()
                        .await
                        .unwrap()
                        .unwrap()
                        .to_text()
                        .unwrap()
                        .contains("hello"));
                    socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"ssh-fixture","features":{}}}})
                                .to_string(),
                        ))
                        .await
                        .unwrap();
                    if last {
                        while let Some(Ok(_)) = socket.next().await {}
                    } else {
                        socket.close(None).await.unwrap();
                    }
                }
            });
        });
        let port = port_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let stub = directory.join("ssh");
        let spawns = directory.join("spawns");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/bash\necho $$ >> {}\nexec 3<>/dev/tcp/127.0.0.1/{port}\ncat <&3 &\ncat >&3\n",
                spawns.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::super::ssh::set_test_program(Some((
            stub.to_string_lossy().into_owned(),
            Some(directory.to_string_lossy().into_owned()),
        )));
        let _guard = ProgramGuard;

        let target = crate::hosts::target::parse_host_target("ssh://stub-host").unwrap();
        let client = match HostClient::start(ConnectInput {
            target,
            expected_server_id: Some("ssh-fixture".into()),
        }) {
            Ok(client) => client,
            Err(error) => panic!("host client start failed: {error:#}"),
        };
        let events = client.events.clone();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv_blocking() {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });
        let mut identities = Vec::new();
        let timeout = std::time::Duration::from_secs(15);
        loop {
            match event_rx.recv_timeout(timeout) {
                Ok(HostRuntimeEvent::Connected(info)) => {
                    assert_eq!(info.server_id, "ssh-fixture");
                    identities.push(info.server_id);
                    if identities.len() == 2 {
                        break;
                    }
                }
                Ok(HostRuntimeEvent::Disconnected(_)) => {}
                Ok(other) => panic!(
                    "unexpected host event ({} connected so far): {}",
                    identities.len(),
                    match &other {
                        HostRuntimeEvent::Connected(_) => "Connected",
                        HostRuntimeEvent::ConnectedWithoutIdentity(_) => "ConnectedWithoutIdentity",
                        HostRuntimeEvent::IdentityMismatch(_) => "IdentityMismatch",
                        HostRuntimeEvent::Event(_) => "Event",
                        HostRuntimeEvent::Disconnected(error) => error,
                    }
                ),
                Err(error) => panic!(
                    "the supervisor never reconnected ({} connected so far): {error}",
                    identities.len()
                ),
            }
        }
        assert_eq!(identities, ["ssh-fixture", "ssh-fixture"]);
        // Both sessions ran through fresh ssh children of the one tunnel.
        let mut children = Vec::new();
        for _ in 0..50 {
            if let Ok(file) = std::fs::File::open(&spawns) {
                children = std::io::BufReader::new(file)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>();
                if children.len() >= 2 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        children.sort();
        children.dedup();
        assert!(
            children.len() >= 2,
            "the reconnect must spawn a second ssh child, saw {children:?}"
        );
        client.close();
        let _ = std::fs::remove_dir_all(&directory);
    }
}

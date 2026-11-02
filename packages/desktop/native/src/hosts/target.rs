//! The shared saved-host and CLI connection grammar
//! (`@getpaseo/protocol/host-target`): `host:port`, `ws(s)://`, `tcp://`,
//! `unix://`, `pipe://`, `ssh://` and pairing URLs. Errors never echo
//! credential-bearing input.

use anyhow::{Context as _, Result, bail};

use super::endpoints::{
    build_daemon_websocket_url, normalize_host_port, should_use_tls_for_default_hosted_relay,
    validated_endpoint,
};
use super::offer::{
    ParsedRelayConnectionUri, parse_connection_offer_from_url, parse_relay_connection_uri,
};

pub const DEFAULT_SSH_DAEMON_PORT: u16 = 6767;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTransportTarget {
    pub host: String,
    pub ssh_port: Option<u16>,
    pub daemon_port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostTarget {
    Ssh {
        ssh: SshTransportTarget,
        password: Option<String>,
        description: String,
    },
    Socket {
        url: String,
        /// Unix socket or named pipe path; `None` for TCP targets.
        socket_path: Option<String>,
        /// True when the path is a Windows named pipe.
        pipe: bool,
        password: Option<String>,
        offer: Option<super::offer::ConnectionOffer>,
        description: String,
    },
}

impl HostTarget {
    pub fn description(&self) -> &str {
        match self {
            HostTarget::Ssh { description, .. } => description,
            HostTarget::Socket { description, .. } => description,
        }
    }

    pub fn password(&self) -> Option<&str> {
        match self {
            HostTarget::Ssh { password, .. } => password.as_deref(),
            HostTarget::Socket { password, .. } => password.as_deref(),
        }
    }

    /// Saved profiles keep the daemon password outside the connection grammar:
    /// socket and pipe grammars have no query string to carry one. An explicit
    /// password here overrides one parsed from the grammar.
    pub fn with_password(mut self, password: Option<String>) -> Self {
        let Some(password) = password.filter(|password| !password.is_empty()) else {
            return self;
        };
        match &mut self {
            HostTarget::Ssh {
                password: target, ..
            }
            | HostTarget::Socket {
                password: target, ..
            } => *target = Some(password),
        }
        self
    }
}

/// The shared grammar's single failure message. Details about the input are
/// dropped so a pasted credential can never reappear in an error.
fn invalid_connection() -> anyhow::Error {
    anyhow::format_err!(
        "Invalid host connection. Use host:port, ws(s)://, tcp://, unix://, pipe://, ssh:// or a pairing URL."
    )
}

pub fn validate_port(value: u16, label: &str) -> Result<u16> {
    if value == 0 {
        bail!("{label} must be between 1 and 65535");
    }
    Ok(value)
}

pub fn validate_ssh_host(host: &str) -> Result<String> {
    let normalized = host.trim();
    if normalized.is_empty()
        || normalized.chars().any(char::is_whitespace)
        || normalized.starts_with('-')
    {
        bail!("SSH host is invalid");
    }
    Ok(normalized.to_owned())
}

pub fn parse_host_target(input: &str) -> Result<HostTarget> {
    parse_target(input.trim()).map_err(|error| {
        if error.to_string().starts_with("Invalid host connection") {
            error
        } else {
            invalid_connection()
        }
    })
}

fn parse_target(input: &str) -> Result<HostTarget> {
    if input.is_empty() {
        return Err(invalid_connection());
    }
    if let Some(rest) = input.strip_prefix("ssh://") {
        return ssh_target(rest);
    }
    if input.starts_with("relay://") || input.contains("#connect=") {
        let relay = parse_relay_connection_uri(input).map_err(|_| invalid_connection())?;
        return relay_target(relay);
    }
    if let Some(offer) = parse_connection_offer_from_url(input).map_err(|_| invalid_connection())? {
        return relay_target(ParsedRelayConnectionUri {
            offer,
            password: None,
        });
    }
    if input.starts_with("unix://")
        || input.starts_with("pipe://")
        || input.starts_with('/')
        || input.starts_with('~')
        || input.starts_with("\\\\.\\pipe\\")
    {
        let pipe = input.starts_with("pipe://") || input.starts_with("\\\\.\\pipe\\");
        let socket_path = input
            .strip_prefix("unix://")
            .or_else(|| input.strip_prefix("pipe://"))
            .unwrap_or(input);
        if socket_path.is_empty() || socket_path == "~" {
            return Err(invalid_connection());
        }
        return Ok(HostTarget::Socket {
            url: if pipe {
                "ws://localhost/ws".into()
            } else {
                format!("ws+unix://{socket_path}:/ws")
            },
            socket_path: Some(socket_path.to_owned()),
            pipe,
            password: None,
            offer: None,
            description: format!("{}://{socket_path}", if pipe { "pipe" } else { "unix" }),
        });
    }
    if let Some(rest) = input.strip_prefix("tcp://") {
        return tcp_target(rest);
    }
    if input.starts_with("ws://") || input.starts_with("wss://") {
        return websocket_target(input);
    }
    let candidate = if input.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("127.0.0.1:{input}")
    } else {
        input.to_owned()
    };
    let endpoint = normalize_host_port(&candidate).map_err(|_| invalid_connection())?;
    validated_endpoint(&endpoint)?;
    Ok(HostTarget::Socket {
        url: build_daemon_websocket_url(&endpoint, false).map_err(|_| invalid_connection())?,
        socket_path: None,
        pipe: false,
        password: None,
        offer: None,
        description: format!("tcp://{endpoint}"),
    })
}

fn relay_target(relay: ParsedRelayConnectionUri) -> Result<HostTarget> {
    let password = relay.password;
    let offer = relay.offer;
    let use_tls = offer
        .relay
        .use_tls
        .unwrap_or_else(|| should_use_tls_for_default_hosted_relay(&offer.relay.endpoint));
    let url = super::endpoints::build_relay_websocket_url(super::endpoints::RelayUrlParams {
        endpoint: &offer.relay.endpoint,
        use_tls,
        server_id: &offer.server_id,
        role: "client",
        version: None,
        connection_id: None,
    })
    .map_err(|_| invalid_connection())?;
    let description = format!(
        "relay://{}/{}",
        offer.relay.endpoint,
        urlencode_component(&offer.server_id)
    );
    Ok(HostTarget::Socket {
        url,
        socket_path: None,
        pipe: false,
        password,
        offer: Some(offer),
        description,
    })
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

fn tcp_target(rest: &str) -> Result<HostTarget> {
    if rest.is_empty() {
        return Err(invalid_connection());
    }
    let (authority, query) = match rest.find('?') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, ""),
    };
    // A TCP URI has no path or fragment.
    if authority.contains('/') || rest.contains('#') {
        return Err(invalid_connection());
    }
    let mut use_tls = false;
    let mut password: Option<String> = None;
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        let name = urldecode(name);
        let value = urldecode(value);
        match name.as_str() {
            "ssl" => use_tls = value == "true",
            "password" => password = Some(value),
            _ => {}
        }
    }
    let endpoint = normalize_host_port(authority).map_err(|_| invalid_connection())?;
    validated_endpoint(&endpoint)?;
    Ok(HostTarget::Socket {
        url: build_daemon_websocket_url(&endpoint, use_tls).map_err(|_| invalid_connection())?,
        socket_path: None,
        pipe: false,
        password,
        offer: None,
        description: format!("tcp://{endpoint}"),
    })
}

fn urldecode(value: &str) -> String {
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
                match u8::from_str_radix(
                    std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                    16,
                ) {
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

fn websocket_target(input: &str) -> Result<HostTarget> {
    let rest = input
        .strip_prefix("ws://")
        .or_else(|| input.strip_prefix("wss://"))
        .context("unreachable websocket scheme")?;
    if rest.contains('#') || rest.contains('@') {
        return Err(invalid_connection());
    }
    let (authority_and_path, query) = match rest.find('?') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, ""),
    };
    let (authority, path) = match authority_and_path.find('/') {
        Some(index) => (&authority_and_path[..index], &authority_and_path[index..]),
        None => (authority_and_path, "/"),
    };
    if authority.is_empty() {
        return Err(invalid_connection());
    }
    let mut password: Option<String> = None;
    let mut kept_query: Vec<String> = Vec::new();
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        if urldecode(name) == "password" {
            password = Some(urldecode(value));
        } else {
            kept_query.push(part.to_owned());
        }
    }
    let path = if path == "/" { "/ws" } else { path };
    let scheme = &input[..input.find(':').context("unreachable scheme")?];
    let mut url = format!("{scheme}://{authority}{path}");
    if !kept_query.is_empty() {
        url.push('?');
        url.push_str(&kept_query.join("&"));
    }
    let host = if authority.starts_with('[') {
        authority
            .find(']')
            .and_then(|close| authority.get(1..close))
            .unwrap_or(authority)
            .to_owned()
    } else {
        authority
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(authority)
            .to_owned()
    };
    Ok(HostTarget::Socket {
        url,
        socket_path: None,
        pipe: false,
        password,
        offer: None,
        description: host,
    })
}

fn ssh_target(rest: &str) -> Result<HostTarget> {
    let (before_fragment, _) = match rest.find('#') {
        Some(index) => (&rest[..index], ()),
        None => (rest, ()),
    };
    let (authority_and_path, query) = match before_fragment.find('?') {
        Some(index) => (&before_fragment[..index], &before_fragment[index + 1..]),
        None => (before_fragment, ""),
    };
    // A SSH URI has no path.
    if authority_and_path.contains('/') {
        return Err(invalid_connection());
    }
    let (userinfo, host_and_port) = match authority_and_path.find('@') {
        Some(index) => (
            Some(&authority_and_path[..index]),
            &authority_and_path[index + 1..],
        ),
        None => (None, authority_and_path),
    };
    if let Some(userinfo) = userinfo
        && userinfo.contains(':')
    {
        // ssh://user:secret@host would embed an SSH password in the URI.
        return Err(invalid_connection());
    }
    let (host, ssh_port) = match host_and_port.rfind(':') {
        Some(index) => {
            let port = &host_and_port[index + 1..];
            let port: u16 = port
                .parse()
                .ok()
                .filter(|port| (1..=65535).contains(port))
                .ok_or_else(invalid_connection)?;
            (&host_and_port[..index], Some(port))
        }
        None => (host_and_port, None),
    };
    // IPv6 literals arrive bracketed from the URI form.
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let mut daemon_port: Option<u16> = None;
    let mut password: Option<String> = None;
    let mut password_count = 0;
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        match urldecode(name).as_str() {
            "daemonPort" => {
                daemon_port = Some(
                    urldecode(value)
                        .parse()
                        .ok()
                        .filter(|port| (1..=65535).contains(port))
                        .ok_or_else(invalid_connection)?,
                );
            }
            "password" => {
                password_count += 1;
                if password_count > 1 {
                    return Err(invalid_connection());
                }
                password = Some(urldecode(value));
            }
            _ => return Err(invalid_connection()),
        }
    }
    let username = userinfo.map(urldecode).unwrap_or_default();
    let host = validate_ssh_host(&format!(
        "{username}{}{host}",
        if username.is_empty() { "" } else { "@" }
    ))
    .map_err(|_| invalid_connection())?;
    let daemon_port = daemon_port.unwrap_or(DEFAULT_SSH_DAEMON_PORT);
    Ok(HostTarget::Ssh {
        ssh: SshTransportTarget {
            host,
            ssh_port,
            daemon_port,
        },
        password,
        description: format!("ssh://{host_and_port} (daemon port {daemon_port})"),
    })
}

pub fn build_ssh_tunnel_args(target: &SshTransportTarget) -> Result<Vec<String>> {
    let host = validate_ssh_host(&target.host)?;
    let daemon_port = validate_port(target.daemon_port, "Daemon port")?;
    let mut args = [
        "-T".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ConnectTimeout=10".to_owned(),
        "-o".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
    ]
    .into_iter()
    .collect::<Vec<_>>();
    if let Some(ssh_port) = target.ssh_port {
        args.push("-p".into());
        args.push(validate_port(ssh_port, "SSH port")?.to_string());
    }
    args.push("-W".into());
    args.push(format!("127.0.0.1:{daemon_port}"));
    args.push(host);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_safe_display_text() {
        let cases = [
            ("127.0.0.1:6767", "tcp://127.0.0.1:6767"),
            ("4321", "tcp://127.0.0.1:4321"),
            ("[::1]:4321", "tcp://[::1]:4321"),
            ("unix:///tmp/paseo.sock", "unix:///tmp/paseo.sock"),
            ("pipe://\\\\.\\pipe\\paseo", "pipe://\\\\.\\pipe\\paseo"),
            ("wss://example.com/custom?token=secret", "example.com"),
            (
                "tcp://example.com:6767?password=secret",
                "tcp://example.com:6767",
            ),
        ];
        for (connection, description) in cases {
            assert_eq!(
                parse_host_target(connection).unwrap().description(),
                description,
                "{connection}"
            );
        }
    }

    #[test]
    fn separates_ssh_daemon_credentials_from_display_text() {
        let target =
            parse_host_target("ssh://deploy@build-box:2222?daemonPort=7777&password=secret")
                .unwrap();
        match target {
            HostTarget::Ssh {
                ssh,
                password,
                description,
            } => {
                assert_eq!(ssh.host, "deploy@build-box");
                assert_eq!(ssh.ssh_port, Some(2222));
                assert_eq!(ssh.daemon_port, 7777);
                assert_eq!(password.as_deref(), Some("secret"));
                assert_eq!(description, "ssh://build-box:2222 (daemon port 7777)");
            }
            other => panic!("expected ssh target, got {other:?}"),
        }
    }

    #[test]
    fn retains_ssh_aliases_and_encoded_usernames_without_leaking_them() {
        let target = parse_host_target("build-box.example.:6767").unwrap();
        assert_eq!(target.description(), "tcp://build-box.example.:6767");
        let target = parse_host_target("ssh://deploy@my_ssh-alias").unwrap();
        match target {
            HostTarget::Ssh { ssh, .. } => assert_eq!(ssh.host, "deploy@my_ssh-alias"),
            other => panic!("expected ssh target, got {other:?}"),
        }
        let target = parse_host_target("ssh://alice%3Aexample-secret@my-alias").unwrap();
        // The description uses the URI hostname; the decoded username stays out of it.
        assert_eq!(target.description(), "ssh://my-alias (daemon port 6767)");
    }

    #[test]
    fn rejects_invalid_syntax_without_echoing_the_input() {
        for connection in [
            "",
            "ssh://-bad",
            "ssh://user:secret@host",
            "ssh://host?unknown=secret",
            "unix://",
            "tcp://host:0",
            "C:\\Users\\socket",
            "alice:example-secret@localhost:6767",
            "not-a-scheme://localhost:6767",
            "localhost/path:6767",
            "localhost?password=example-secret:6767",
            "localhost#example-secret:6767",
            "tcp://localhost/path:6767",
            "relay://alice:example-secret@localhost:6767/server?key=private",
        ] {
            let error = parse_host_target(connection).unwrap_err().to_string();
            assert!(
                error.starts_with("Invalid host connection"),
                "{connection}: {error}"
            );
            assert!(!error.contains("secret"), "{connection}");
        }
    }

    #[test]
    fn parses_socket_targets_with_pipe_semantics() {
        let target = parse_host_target("unix:///tmp/paseo.sock").unwrap();
        match target {
            HostTarget::Socket {
                url,
                socket_path,
                pipe,
                ..
            } => {
                assert_eq!(url, "ws+unix:///tmp/paseo.sock:/ws");
                assert_eq!(socket_path.as_deref(), Some("/tmp/paseo.sock"));
                assert!(!pipe);
            }
            other => panic!("unexpected target {other:?}"),
        }
        let target = parse_host_target("pipe://\\\\.\\pipe\\paseo").unwrap();
        match target {
            HostTarget::Socket { url, pipe, .. } => {
                assert_eq!(url, "ws://localhost/ws");
                assert!(pipe);
            }
            other => panic!("unexpected target {other:?}"),
        }
    }

    #[test]
    fn keeps_websocket_paths_and_strips_only_the_password_parameter() {
        let target = parse_host_target("wss://example.com/custom?token=secret").unwrap();
        match target {
            HostTarget::Socket { url, password, .. } => {
                assert_eq!(url, "wss://example.com/custom?token=secret");
                assert_eq!(password, None);
            }
            other => panic!("unexpected target {other:?}"),
        }
        let target = parse_host_target("ws://example.com?password=secret").unwrap();
        match target {
            HostTarget::Socket { url, password, .. } => {
                assert_eq!(url, "ws://example.com/ws");
                assert_eq!(password.as_deref(), Some("secret"));
            }
            other => panic!("unexpected target {other:?}"),
        }
    }

    #[test]
    fn decodes_tls_and_credential_query_parameters_before_interpreting_them() {
        let target = parse_host_target("tcp://box:7777?ssl=%74rue&password=a%26b").unwrap();
        match target {
            HostTarget::Socket { url, password, .. } => {
                assert_eq!(url, "wss://box:7777/ws");
                assert_eq!(password.as_deref(), Some("a&b"));
            }
            other => panic!("unexpected target {other:?}"),
        }
    }

    #[test]
    fn applies_a_saved_password_over_one_parsed_from_the_grammar() {
        let target = parse_host_target("unix:///tmp/paseo.sock")
            .unwrap()
            .with_password(Some("a&b=c#d%".into()));
        assert_eq!(target.password(), Some("a&b=c#d%"));
        let target = parse_host_target("ssh://build-box")
            .unwrap()
            .with_password(None);
        assert_eq!(target.password(), None);
    }

    #[test]
    fn builds_the_non_interactive_stdio_tunnel_args() {
        let args = build_ssh_tunnel_args(&SshTransportTarget {
            host: "deploy@build-box".into(),
            ssh_port: Some(2222),
            daemon_port: 7777,
        })
        .unwrap();
        assert_eq!(
            args,
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-p",
                "2222",
                "-W",
                "127.0.0.1:7777",
                "deploy@build-box",
            ]
        );
    }

    #[test]
    fn rejects_unsafe_ssh_hosts_and_ports() {
        assert!(
            build_ssh_tunnel_args(&SshTransportTarget {
                host: "-oProxyCommand=bad".into(),
                ssh_port: None,
                daemon_port: 6767,
            })
            .is_err()
        );
        assert!(
            build_ssh_tunnel_args(&SshTransportTarget {
                host: "bad host".into(),
                ssh_port: None,
                daemon_port: 6767,
            })
            .is_err()
        );
        assert!(
            build_ssh_tunnel_args(&SshTransportTarget {
                host: "host".into(),
                ssh_port: None,
                daemon_port: 0,
            })
            .is_err()
        );
    }
}

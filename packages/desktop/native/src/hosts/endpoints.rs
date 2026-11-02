//! Endpoint grammar shared with the daemon and CLI (`@getpaseo/protocol/daemon-endpoints`).
//! The native client must accept exactly the same saved-host and CLI inputs.

use anyhow::{Result, bail};

pub const DEFAULT_RELAY_ENDPOINT: &str = "relay.paseo.sh:443";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPortParts {
    pub host: String,
    pub port: u16,
    pub is_ipv6: bool,
}

pub fn parse_port(value: &str, context: &str) -> Result<u16> {
    if value.is_empty() || value.len() > 5 || !value.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{context}: port must be between 1 and 65535");
    }
    let port: u32 = value
        .parse()
        .map_err(|_| anyhow::format_err!("{context}: port must be between 1 and 65535"))?;
    u16::try_from(port)
        .ok()
        .filter(|port| (1..=65535).contains(port))
        .ok_or_else(|| anyhow::format_err!("{context}: port must be between 1 and 65535"))
}

/// A bare endpoint has exactly one authority: `host:port` or `[ipv6]:port`.
pub fn parse_host_port(input: &str) -> Result<HostPortParts> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("Host is required");
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| anyhow::format_err!("Invalid host:port (expected [::1]:6767)"))?;
        let host = rest[..close].trim();
        if host.is_empty() {
            bail!("Host is required");
        }
        let port = rest[close + 1..]
            .strip_prefix(':')
            .ok_or_else(|| anyhow::format_err!("Invalid host:port (expected [::1]:6767)"))?;
        return Ok(HostPortParts {
            host: host.to_owned(),
            port: parse_port(port, "Invalid host:port")?,
            is_ipv6: true,
        });
    }
    let Some((host, port)) = trimmed.rsplit_once(':') else {
        bail!("Invalid host:port (expected localhost:6767)");
    };
    let host = host.trim();
    if host.is_empty() {
        bail!("Host is required");
    }
    Ok(HostPortParts {
        host: host.to_owned(),
        port: parse_port(port, "Invalid host:port")?,
        is_ipv6: false,
    })
}

/// A bare endpoint has exactly one authority: hostname/IPv4 or bracketed IPv6,
/// one port. Matches `@getpaseo/protocol/host-target.ts#validatedEndpoint`.
pub fn validated_endpoint(input: &str) -> Result<()> {
    let parts = parse_host_port(input)?;
    if parts.is_ipv6 {
        return Ok(());
    }
    let mut chars = parts.host.chars();
    let valid = match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() || first == '_' => {
            chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        }
        _ => false,
    };
    if !valid {
        anyhow::bail!("Invalid endpoint");
    }
    Ok(())
}

pub fn normalize_host_port(input: &str) -> Result<String> {
    let parts = parse_host_port(input)?;
    Ok(if parts.is_ipv6 {
        format!("[{}]:{}", parts.host, parts.port)
    } else {
        format!("{}:{}", parts.host, parts.port)
    })
}

pub fn normalize_loopback_to_localhost(endpoint: &str) -> Result<String> {
    let parts = parse_host_port(endpoint)?;
    let host = parts.host.as_str();
    let loopback = if parts.is_ipv6 {
        matches!(host, "::1" | "::")
    } else {
        host == "127.0.0.1" || host == "0.0.0.0"
    };
    Ok(if loopback {
        format!("localhost:{}", parts.port)
    } else {
        endpoint.to_owned()
    })
}

pub fn derive_label_from_endpoint(endpoint: &str) -> String {
    match parse_host_port(endpoint) {
        Ok(parts) if !parts.host.is_empty() => parts.host,
        _ => "Unnamed Host".into(),
    }
}

pub fn build_daemon_websocket_url(endpoint: &str, use_tls: bool) -> Result<String> {
    let parts = parse_host_port(endpoint)?;
    let host_part = if parts.is_ipv6 {
        format!("[{}]", parts.host)
    } else {
        parts.host.clone()
    };
    Ok(format!(
        "{}://{host_part}:{}/ws",
        if use_tls { "wss" } else { "ws" },
        parts.port
    ))
}

/// Percent-encodes a query value the way `URLSearchParams.set` does, so a
/// relay server's URL parser reads back the same value.
pub fn encode_query_component(value: &str) -> String {
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

pub fn build_relay_websocket_url(params: RelayUrlParams) -> Result<String> {
    let parts = parse_host_port(params.endpoint)?;
    let host_part = if parts.is_ipv6 {
        format!("[{}]", parts.host)
    } else {
        parts.host.clone()
    };
    let version = normalize_relay_protocol_version(params.version)?;
    let mut url = format!(
        "{}://{host_part}:{}/ws?serverId={}&role={}&v={version}",
        if params.use_tls { "wss" } else { "ws" },
        parts.port,
        encode_query_component(params.server_id),
        params.role,
    );
    if let Some(connection_id) = params.connection_id {
        url.push_str("&connectionId=");
        url.push_str(&encode_query_component(connection_id));
    }
    Ok(url)
}

pub struct RelayUrlParams<'a> {
    pub endpoint: &'a str,
    pub use_tls: bool,
    pub server_id: &'a str,
    pub role: &'a str,
    pub version: Option<&'a str>,
    pub connection_id: Option<&'a str>,
}

pub fn normalize_relay_protocol_version(value: Option<&str>) -> Result<&'static str> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok("2"),
        Some("1") => Ok("1"),
        Some("2") => Ok("2"),
        Some(_) => bail!("Relay version must be \"1\" or \"2\""),
    }
}

/// Default hosted-relay ports carried no TLS flag in older offers.
pub fn should_use_tls_for_default_hosted_relay(endpoint: &str) -> bool {
    parse_host_port(endpoint).is_ok_and(|parts| parts.port == 443)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_tcp_host_and_port() {
        assert_eq!(
            normalize_host_port("localhost:6767").unwrap(),
            "localhost:6767"
        );
        assert_eq!(
            normalize_host_port("  localhost:6767 ").unwrap(),
            "localhost:6767"
        );
    }

    #[test]
    fn round_trips_ipv6_host() {
        assert_eq!(
            normalize_host_port("[2001:db8::1]:4321").unwrap(),
            "[2001:db8::1]:4321"
        );
    }

    #[test]
    fn rejects_missing_or_invalid_ports() {
        assert!(normalize_host_port("localhost").is_err());
        assert!(normalize_host_port("localhost:").is_err());
        assert!(normalize_host_port("localhost:0").is_err());
        assert!(normalize_host_port("localhost:99999").is_err());
        assert!(normalize_host_port("localhost:6767x").is_err());
    }

    #[test]
    fn normalizes_loopback_variants_to_localhost() {
        assert_eq!(
            normalize_loopback_to_localhost("127.0.0.1:6767").unwrap(),
            "localhost:6767"
        );
        assert_eq!(
            normalize_loopback_to_localhost("0.0.0.0:6767").unwrap(),
            "localhost:6767"
        );
        assert_eq!(
            normalize_loopback_to_localhost("[::1]:6767").unwrap(),
            "localhost:6767"
        );
        assert_eq!(
            normalize_loopback_to_localhost("example.com:6767").unwrap(),
            "example.com:6767"
        );
    }

    #[test]
    fn builds_daemon_websocket_urls() {
        assert_eq!(
            build_daemon_websocket_url("localhost:6767", false).unwrap(),
            "ws://localhost:6767/ws"
        );
        assert_eq!(
            build_daemon_websocket_url("example.com:6767", true).unwrap(),
            "wss://example.com:6767/ws"
        );
        assert_eq!(
            build_daemon_websocket_url("[::1]:4321", false).unwrap(),
            "ws://[::1]:4321/ws"
        );
    }

    #[test]
    fn builds_relay_client_urls_with_v2_by_default() {
        assert_eq!(
            build_relay_websocket_url(RelayUrlParams {
                endpoint: "relay.paseo.sh:443",
                use_tls: true,
                server_id: "srv_test",
                role: "client",
                version: None,
                connection_id: None,
            })
            .unwrap(),
            "wss://relay.paseo.sh:443/ws?serverId=srv_test&role=client&v=2"
        );
        assert!(
            build_relay_websocket_url(RelayUrlParams {
                endpoint: "relay.paseo.sh:443",
                use_tls: true,
                server_id: "srv",
                role: "client",
                version: Some("3"),
                connection_id: None,
            })
            .is_err()
        );
    }

    #[test]
    fn encodes_query_values_without_leaving_reserved_characters() {
        assert_eq!(
            encode_query_component("a&b=c#d% e+f"),
            "a%26b%3Dc%23d%25%20e%2Bf"
        );
    }

    #[test]
    fn defaults_relay_tls_to_port_443_only() {
        assert!(should_use_tls_for_default_hosted_relay(
            "relay.paseo.sh:443"
        ));
        assert!(should_use_tls_for_default_hosted_relay("[::1]:443"));
        assert!(!should_use_tls_for_default_hosted_relay(
            "relay.example.com:8443"
        ));
        assert!(!should_use_tls_for_default_hosted_relay("garbage"));
    }

    #[test]
    fn derives_labels_from_host_parts() {
        assert_eq!(derive_label_from_endpoint("build-box:6767"), "build-box");
        assert_eq!(derive_label_from_endpoint("[::1]:4321"), "::1");
        assert_eq!(derive_label_from_endpoint("garbage"), "Unnamed Host");
    }
}

//! Protected relay endpoint security for the native client.
//!
//! This is the Rust twin of `packages/relay/src/secure-channel.ts` (protected
//! mode only): `Noise_NK_25519_ChaChaPoly_SHA256` bound to the daemon's static
//! key, with authenticated record framing. The byte-level mapping is owned by
//! `docs/relay-endpoint-security.md`. The native client is protected-only: a
//! legacy ready is refused with close 4002 and there is no insecure fallback.
//!
//! Handshake messages travel byte-exact (base64) inside the existing
//! `e2ee_hello` / `e2ee_ready` JSON envelopes. One application frame becomes
//! one record; see [`records`].

pub mod channel;
pub mod records;

pub use channel::{
    ClientHandshake, NegotiatedParams, PlaintextTraffic, ProtectedChannel, RelaySecurity,
    classify_plaintext_text,
};
pub use records::{DecodedFrame, max_relay_frame_bytes};

/// The relay replaces the daemon WebSocket, so wire limits follow the relay's
/// WebSocket message cap (`@getpaseo/protocol/websocket-message-limit`).
pub const MAX_RELAY_WEBSOCKET_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// Client-side close when a protected handshake fails. Browser-safe (4000-4999).
pub const PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE: u16 = 4002;
/// Client-side close when an inbound record fails authentication or framing.
pub const PROTECTED_RECORD_FAILED_CLOSE_CODE: u16 = 4003;

pub const NOISE_PROTOCOL_NAME: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
pub const NOISE_PROLOGUE: &[u8] = b"paseo-relay-e2ee/1";

/// The application-frame kind a record carries, preserved on the wire the same
/// way as legacy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Text,
    Binary,
}

/// A transport-security failure that carries its own close code, so the
/// supervisor stops retrying on 4002 instead of guessing from error strings.
/// Reasons are static text and never contain key material or transcripts.
#[derive(Debug, Clone)]
pub struct TransportSecurityError {
    pub close_code: u16,
    pub reason: &'static str,
}

impl TransportSecurityError {
    pub fn handshake(reason: &'static str) -> Self {
        Self {
            close_code: PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE,
            reason,
        }
    }
    pub fn record(reason: &'static str) -> Self {
        Self {
            close_code: PROTECTED_RECORD_FAILED_CLOSE_CODE,
            reason,
        }
    }
}

impl std::fmt::Display for TransportSecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "relay endpoint security failed (close {}): {}",
            self.close_code, self.reason
        )
    }
}

impl std::error::Error for TransportSecurityError {}

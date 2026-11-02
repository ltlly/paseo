//! Client-side protected secure channel: the `e2ee_hello` / `e2ee_ready`
//! envelopes, parameter negotiation, and the record-protected transport, mirroring
//! the protected paths of `packages/relay/src/secure-channel.ts`.
//!
//! The native client is protected-only. A ready without a `protected` field, a
//! present but invalid protected message, or a selection the client did not
//! offer is a fatal [`TransportSecurityError`] with close 4002 — there is no
//! fallback to legacy. Record failures close with 4003; the supervisor
//! reconnects with a fresh handshake (fresh ephemeral) for those.

use anyhow::{Context, Result, ensure};
use snow::{Builder, HandshakeState, TransportState};

use crate::relay::records::{base64_decode, base64_encode, decode_record, encode_record};
use crate::relay::{NOISE_PROLOGUE, NOISE_PROTOCOL_NAME, TransportSecurityError};

use snow::resolvers::{CryptoResolver, DefaultResolver, FallbackResolver, RingResolver};
use snow::types::{Dh, Random};

const PROTECTED_ENVELOPE_VERSION: u64 = 1;
const PARAMS_VERSION: u8 = 0x01;
const PARAM_FLAG_BINARY_CIPHERTEXT: u8 = 0x01;
const PARAM_FLAGS_KNOWN: u8 = PARAM_FLAG_BINARY_CIPHERTEXT;
/// e (32) + params (2) + tag (16).
const HANDSHAKE_MESSAGE_BYTES: usize = 50;
const NOISE_MAX_MESSAGE_BYTES: usize = 65535;

/// The daemon identity a relay connection authenticates against: the X25519
/// static public key that pairing distributes (`daemon-keypair.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySecurity {
    pub daemon_public_key_b64: String,
}

impl RelaySecurity {
    pub fn new(daemon_public_key_b64: String) -> Result<Self> {
        let key = base64_public_key(&daemon_public_key_b64)
            .context("relay host daemon public key is invalid")?;
        ensure!(
            key.len() == 32,
            "relay host daemon public key must be 32 bytes"
        );
        Ok(Self {
            daemon_public_key_b64,
        })
    }
}

fn base64_public_key(text: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.decode(text)?)
}

/// The offered/selected parameter payload: `[0x01 version][flags]`.
pub(crate) fn params_bytes(binary_ciphertext: bool) -> [u8; 2] {
    [
        PARAMS_VERSION,
        u8::from(binary_ciphertext) & PARAM_FLAG_BINARY_CIPHERTEXT,
    ]
}

/// The offered/selected parameter payload carried inside the Noise handshake
/// messages: `[0x01 version][flags]`, flag bit 0 = binaryCiphertext.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegotiatedParams {
    pub binary_ciphertext: bool,
}

pub(crate) fn decode_params(payload: &[u8]) -> Result<NegotiatedParams, TransportSecurityError> {
    if payload.len() != 2 {
        return Err(TransportSecurityError::handshake(
            "invalid parameter payload",
        ));
    }
    if payload[0] != PARAMS_VERSION {
        return Err(TransportSecurityError::handshake(
            "unsupported parameter version",
        ));
    }
    if payload[1] & !PARAM_FLAGS_KNOWN != 0 {
        return Err(TransportSecurityError::handshake(
            "reserved parameter flag set",
        ));
    }
    Ok(NegotiatedParams {
        binary_ciphertext: payload[1] & PARAM_FLAG_BINARY_CIPHERTEXT != 0,
    })
}

/// DH wrapper that fails closed on an all-zero shared secret. snow's default
/// X25519 returns the raw scalar product and accepts non-contributing keys
/// (an all-zero or low-order input), which would let a relay without the
/// daemon static private key produce a valid ready. This mirrors the TS check
/// in `packages/relay/src/noise/index.ts`: every handshake DH output — es in
/// message 1 and ee in message 2 alike — must contribute.
struct FailClosedDh {
    inner: Box<dyn Dh>,
}

impl Dh for FailClosedDh {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn pub_len(&self) -> usize {
        self.inner.pub_len()
    }
    fn priv_len(&self) -> usize {
        self.inner.priv_len()
    }
    fn set(&mut self, privkey: &[u8]) {
        self.inner.set(privkey);
    }
    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), snow::Error> {
        self.inner.generate(rng)
    }
    fn pubkey(&self) -> &[u8] {
        self.inner.pubkey()
    }
    fn privkey(&self) -> &[u8] {
        self.inner.privkey()
    }
    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        self.inner.dh(pubkey, out)?;
        if out.iter().all(|&byte| byte == 0) {
            return Err(snow::Error::Dh);
        }
        Ok(())
    }
}

/// The production resolver: ring for RNG/SHA256/ChaChaPoly, rustcrypto
/// curve25519 for DH (ring exposes none; `use-curve25519` supplies the
/// fallback), with the fail-closed DH wrapper on every resolved DH so both
/// handshake parties reject non-contributing keys.
struct FailClosedResolver {
    inner: snow::resolvers::BoxedCryptoResolver,
}

impl CryptoResolver for FailClosedResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        self.inner.resolve_rng()
    }
    fn resolve_dh(&self, choice: &snow::params::DHChoice) -> Option<Box<dyn Dh>> {
        self.inner
            .resolve_dh(choice)
            .map(|dh| Box::new(FailClosedDh { inner: dh }) as Box<dyn Dh>)
    }
    fn resolve_hash(
        &self,
        choice: &snow::params::HashChoice,
    ) -> Option<Box<dyn snow::types::Hash>> {
        self.inner.resolve_hash(choice)
    }
    fn resolve_cipher(
        &self,
        choice: &snow::params::CipherChoice,
    ) -> Option<Box<dyn snow::types::Cipher>> {
        self.inner.resolve_cipher(choice)
    }
}

fn noise_resolver() -> snow::resolvers::BoxedCryptoResolver {
    Box::new(FailClosedResolver {
        inner: Box::new(FallbackResolver::new(
            Box::new(RingResolver),
            Box::new(DefaultResolver),
        )),
    })
}

fn initiator_state(
    daemon_public_key: &[u8],
    seeded_ephemeral: Option<Vec<u8>>,
) -> Result<HandshakeState> {
    let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME
        .parse()
        .context("parse the Noise protocol name")?;
    let mut builder = match seeded_ephemeral {
        // Test injection only (vendored vectors and the TS oracle need a fixed
        // initiator ephemeral); production generates a fresh one.
        Some(seed) => Builder::with_resolver(
            params,
            Box::new(SeededEphemeralResolver {
                seed: std::sync::Mutex::new(Some(seed)),
            }),
        ),
        None => Builder::with_resolver(params, noise_resolver()),
    };
    builder = builder
        .remote_public_key(daemon_public_key)
        .context("relay daemon static key has the wrong length")?
        .prologue(NOISE_PROLOGUE)
        .context("set the Noise prologue")?;
    builder
        .build_initiator()
        .context("build the Noise initiator")
}

/// Serves the seeded ephemeral exactly once, then delegates everything to the
/// production resolver composition.
struct SeededEphemeralResolver {
    seed: std::sync::Mutex<Option<Vec<u8>>>,
}

impl CryptoResolver for SeededEphemeralResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        let production = noise_resolver();
        Some(Box::new(SeededRng {
            seed: self.seed.lock().expect("seed lock").take(),
            fallback: production.resolve_rng()?,
        }))
    }
    fn resolve_dh(&self, choice: &snow::params::DHChoice) -> Option<Box<dyn snow::types::Dh>> {
        noise_resolver().resolve_dh(choice)
    }
    fn resolve_hash(
        &self,
        choice: &snow::params::HashChoice,
    ) -> Option<Box<dyn snow::types::Hash>> {
        noise_resolver().resolve_hash(choice)
    }
    fn resolve_cipher(
        &self,
        choice: &snow::params::CipherChoice,
    ) -> Option<Box<dyn snow::types::Cipher>> {
        noise_resolver().resolve_cipher(choice)
    }
}

struct SeededRng {
    seed: Option<Vec<u8>>,
    fallback: Box<dyn Random>,
}

impl Random for SeededRng {
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), snow::Error> {
        match self.seed.take() {
            Some(seed) if dest.len() >= seed.len() => {
                dest[..seed.len()].copy_from_slice(&seed);
                Ok(())
            }
            Some(_) => Err(snow::Error::Rng),
            None => self.fallback.try_fill_bytes(dest),
        }
    }
}

/// One connection attempt's handshake. The hello text is built once and resent
/// verbatim every second until a ready arrives (the daemon answers identical
/// retries with the identical ready and keeps counters running).
pub struct ClientHandshake {
    noise: Option<HandshakeState>,
    hello_text: String,
    offered: NegotiatedParams,
}

impl ClientHandshake {
    pub fn begin(daemon_public_key_b64: &str) -> Result<Self> {
        let daemon_public_key = base64_public_key(daemon_public_key_b64)
            .context("relay daemon public key is not base64")?;
        ensure!(
            daemon_public_key.len() == 32,
            "relay daemon public key must be 32 bytes"
        );
        Self::from_parts(daemon_public_key, None)
    }

    fn from_parts(daemon_public_key: Vec<u8>, seeded_ephemeral: Option<Vec<u8>>) -> Result<Self> {
        let mut noise = initiator_state(&daemon_public_key, seeded_ephemeral)?;
        let mut message = [0u8; HANDSHAKE_MESSAGE_BYTES];
        let written = noise
            .write_message(&params_bytes(true), &mut message)
            .context("start the Noise handshake")?;
        // Separate legacy ephemeral so an old daemon still completes a legacy
        // handshake; the native client itself never accepts one.
        let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME
            .parse()
            .context("parse the Noise protocol name")?;
        let legacy = Builder::with_resolver(params, noise_resolver())
            .generate_keypair()
            .context("generate the legacy hello ephemeral key")?;
        let hello = serde_json::json!({
            "type": "e2ee_hello",
            "key": base64_encode(&legacy.public),
            "capabilities": {"binaryCiphertext": true},
            "protected": {
                "v": PROTECTED_ENVELOPE_VERSION,
                "message": base64_encode(&message[..written]),
            },
        });
        Ok(Self {
            noise: Some(noise),
            hello_text: hello.to_string(),
            offered: NegotiatedParams {
                binary_ciphertext: true,
            },
        })
    }

    pub fn hello_text(&self) -> &str {
        &self.hello_text
    }

    /// Consumes an inbound text message while handshaking.
    ///
    /// `Ok(None)`: unrelated traffic, keep waiting. `Ok(Some(channel))`: the
    /// daemon authenticated itself; the channel is open. `Err`: fatal 4002.
    pub fn accept_ready(
        &mut self,
        text: &str,
    ) -> Result<Option<ProtectedChannel>, TransportSecurityError> {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) else {
            return Ok(None); // not handshake traffic; ignore while handshaking
        };
        if parsed["type"] != "e2ee_ready" {
            return Ok(None);
        }
        self.open_protected(&parsed).map(Some)
    }

    fn open_protected(
        &mut self,
        ready: &serde_json::Value,
    ) -> Result<ProtectedChannel, TransportSecurityError> {
        let Some(protected) = ready.get("protected") else {
            return Err(TransportSecurityError::handshake(
                "daemon offered only legacy endpoint security",
            ));
        };
        if protected["v"] != PROTECTED_ENVELOPE_VERSION {
            return Err(TransportSecurityError::handshake(
                "unsupported protected envelope version",
            ));
        }
        let Some(message) = protected["message"].as_str().filter(|m| !m.is_empty()) else {
            return Err(TransportSecurityError::handshake(
                "malformed ready envelope",
            ));
        };
        // The handshake state is consumed by the ready, whatever it decides:
        // after this point this attempt is either open or fatally closed.
        let Some(mut noise) = self.noise.take() else {
            return Err(TransportSecurityError::handshake(
                "handshake already consumed",
            ));
        };
        let msg2 = base64_decode(message)
            .map_err(|_| TransportSecurityError::handshake("malformed ready envelope"))?;
        let mut payload = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
        let written = noise.read_message(&msg2, &mut payload).map_err(|_| {
            TransportSecurityError::handshake("protected ready failed authentication")
        })?;
        let selected = decode_params(&payload[..written])?;
        if selected.binary_ciphertext && !self.offered.binary_ciphertext {
            return Err(TransportSecurityError::handshake(
                "daemon selected an unoffered parameter",
            ));
        }
        let transport = noise
            .into_transport_mode()
            .map_err(|_| TransportSecurityError::handshake("handshake state unusable"))?;
        Ok(ProtectedChannel::from_transport(transport, selected))
    }
}

/// Classification of a text WebSocket frame on an open protected channel,
/// mirroring `parseHandshakeEnvelope` in `secure-channel.ts`.
pub enum PlaintextTraffic {
    /// Base64 of a protected record.
    RecordBase64,
    /// A retried hello or a duplicate ready; ignored by the client.
    HandshakeArtifact,
    /// Any other plaintext: the peer is not encrypting. Close with 4003.
    PlaintextApplication,
}

pub fn classify_plaintext_text(text: &str) -> PlaintextTraffic {
    if !text.trim_start().starts_with('{') {
        return PlaintextTraffic::RecordBase64;
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(parsed) if parsed["type"] == "e2ee_hello" || parsed["type"] == "e2ee_ready" => {
            PlaintextTraffic::HandshakeArtifact
        }
        _ => PlaintextTraffic::PlaintextApplication,
    }
}

/// Records over an open Noise transport. Every inbound record is authenticated
/// and framing-checked before delivery; any failure is a 4003 and the caller
/// must close the connection. Send nonces are reserved synchronously inside
/// `encode_frame`, so concurrent senders can never share one.
pub struct ProtectedChannel {
    transport: TransportState,
    params: NegotiatedParams,
}

impl ProtectedChannel {
    pub(crate) fn from_transport(transport: TransportState, params: NegotiatedParams) -> Self {
        Self { transport, params }
    }

    /// True when binary records travel as WebSocket binary frames (rather than
    /// base64 text), as negotiated in the ready's authenticated selection.
    pub fn binary_ciphertext(&self) -> bool {
        self.params.binary_ciphertext
    }

    pub fn encode_frame(
        &mut self,
        kind: crate::relay::FrameKind,
        frame: &[u8],
    ) -> Result<Vec<u8>, TransportSecurityError> {
        encode_record(kind, frame, &mut self.transport)
    }

    pub fn decode_record(
        &mut self,
        record: &[u8],
    ) -> Result<crate::relay::DecodedFrame, TransportSecurityError> {
        decode_record(record, &mut self.transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{
        FrameKind, PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE, PROTECTED_RECORD_FAILED_CLOSE_CODE,
    };

    fn unhex(text: &str) -> Vec<u8> {
        assert_eq!(text.len() % 2, 0, "hex literal must be even");
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    // Same fixed key material as the TS oracle (scripts/relay-noise-oracle.mjs).
    const DAEMON_STATIC_PUBLIC: &str = "jPYaZvWIx+3rGmOHiZ+nwvXzrG71i6DEdcQnkNPho2w=";
    const DAEMON_STATIC_PRIVATE: &[u8] = &[
        0x3f, 0x5c, 0x7a, 0x1b, 0x9d, 0x2e, 0x4f, 0x60, 0x81, 0xa3, 0xc5, 0xe7, 0xf9, 0x02, 0x13,
        0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7a, 0x8b, 0x9c, 0xad, 0xbe, 0xcf, 0xe0, 0xf1, 0x12,
        0x23, 0x3a,
    ];
    const SEEDED_EPHEMERAL_HEX: &str =
        "8d1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f0";

    fn seeded_client() -> ClientHandshake {
        ClientHandshake::from_parts(
            base64_public_key(DAEMON_STATIC_PUBLIC).unwrap(),
            Some(unhex(SEEDED_EPHEMERAL_HEX)),
        )
        .unwrap()
    }

    /// A Rust-only daemon responder over the oracle's static key. Returns the
    /// ready text for the client and the daemon's own open transport, sharing
    /// the single responder whose ephemeral produced message 2.
    fn daemon_respond(client: &mut ClientHandshake) -> (String, ProtectedChannel) {
        let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME.parse().unwrap();
        let mut daemon = Builder::with_resolver(params, noise_resolver())
            .local_private_key(DAEMON_STATIC_PRIVATE)
            .expect("set the daemon static key")
            .prologue(NOISE_PROLOGUE)
            .expect("set the Noise prologue")
            .build_responder()
            .unwrap();
        let hello: serde_json::Value = serde_json::from_str(client.hello_text()).unwrap();
        let msg1 = base64_decode(hello["protected"]["message"].as_str().unwrap()).unwrap();
        let mut payload = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
        let written = daemon.read_message(&msg1, &mut payload).unwrap();
        assert_eq!(&payload[..written], &params_bytes(true));
        let written = daemon
            .write_message(&params_bytes(true), &mut payload)
            .unwrap();
        let ready = serde_json::json!({
            "type": "e2ee_ready",
            "protected": {"v": 1, "message": base64_encode(&payload[..written])},
        })
        .to_string();
        let transport = daemon.into_transport_mode().unwrap();
        (
            ready,
            ProtectedChannel::from_transport(
                transport,
                NegotiatedParams {
                    binary_ciphertext: true,
                },
            ),
        )
    }

    fn open_pair() -> (ProtectedChannel, ProtectedChannel) {
        let mut client = seeded_client();
        let (ready, daemon_channel) = daemon_respond(&mut client);
        let client_channel = client.accept_ready(&ready).unwrap().expect("ready opens");
        (client_channel, daemon_channel)
    }

    #[test]
    fn handshake_and_records_match_the_typescript_oracle_byte_for_byte() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/relay-noise-oracle.json")).unwrap();
        let mut client = ClientHandshake::from_parts(
            unhex(oracle["daemonStaticPublicHex"].as_str().unwrap()),
            Some(unhex(
                oracle["initiatorEphemeralPrivateHex"].as_str().unwrap(),
            )),
        )
        .unwrap();
        // Message 1 and the hello envelope shape.
        let hello: serde_json::Value = serde_json::from_str(client.hello_text()).unwrap();
        assert_eq!(hello["type"], "e2ee_hello");
        assert!(!hello["key"].as_str().unwrap().is_empty());
        assert_eq!(
            hello["capabilities"],
            serde_json::json!({"binaryCiphertext": true})
        );
        assert_eq!(hello["protected"]["v"], 1);
        let msg1 = base64_decode(hello["protected"]["message"].as_str().unwrap()).unwrap();
        assert_eq!(msg1, unhex(oracle["msg1Hex"].as_str().unwrap()));
        // Message 2 authenticates and selects exactly what was offered.
        let ready = serde_json::json!({
            "type": "e2ee_ready",
            "protected": {
                "v": 1,
                "message": base64_encode(&unhex(oracle["msg2Hex"].as_str().unwrap())),
            },
        });
        let mut channel = client.accept_ready(&ready.to_string()).unwrap().unwrap();
        assert!(channel.binary_ciphertext());
        // The daemon side replays the TS responder deterministically: the same
        // static key and the same fixed ephemeral must reproduce msg2 exactly
        // and then decode every TS record.
        let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME.parse().unwrap();
        let mut daemon = Builder::with_resolver(params, noise_resolver())
            .local_private_key(&unhex(oracle["daemonStaticPrivateHex"].as_str().unwrap()))
            .expect("set the daemon static key")
            .fixed_ephemeral_key_for_testing_only(&unhex(
                oracle["responderEphemeralPrivateHex"].as_str().unwrap(),
            ))
            .prologue(NOISE_PROLOGUE)
            .expect("set the Noise prologue")
            .build_responder()
            .unwrap();
        let mut payload = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
        let written = daemon.read_message(&msg1, &mut payload).unwrap();
        assert_eq!(&payload[..written], &params_bytes(true));
        let written = daemon
            .write_message(&params_bytes(true), &mut payload)
            .unwrap();
        assert_eq!(
            &payload[..written],
            &unhex(oracle["msg2Hex"].as_str().unwrap())
        );
        let mut daemon_channel = ProtectedChannel::from_transport(
            daemon.into_transport_mode().unwrap(),
            NegotiatedParams {
                binary_ciphertext: true,
            },
        );
        // Records: snow's initiator encoding must equal the TS encoder's bytes
        // byte for byte, and the TS records must decode on the responder side.
        for (index, expected) in oracle["records"].as_array().unwrap().iter().enumerate() {
            let kind = match expected["kind"].as_str().unwrap() {
                "text" => FrameKind::Text,
                _ => FrameKind::Binary,
            };
            let frame = unhex(expected["frameHex"].as_str().unwrap());
            let record = unhex(expected["recordHex"].as_str().unwrap());
            let encoded = channel.encode_frame(kind, &frame).unwrap();
            assert_eq!(
                encoded, record,
                "encode direction diverged at record {index}"
            );
            let decoded = daemon_channel
                .decode_record(&record)
                .unwrap_or_else(|error| panic!("record {index} decode failed: {error}"));
            assert_eq!(decoded.kind, kind);
            assert_eq!(
                decoded.data, frame,
                "decode direction diverged at record {index}"
            );
        }
    }

    #[test]
    fn rust_only_round_trip_carries_frames_both_ways() {
        let (mut client_channel, mut daemon_channel) = open_pair();
        let frame = b"round trip frame".to_vec();
        let record = client_channel
            .encode_frame(FrameKind::Text, &frame)
            .unwrap();
        let decoded = daemon_channel.decode_record(&record).unwrap();
        assert_eq!(decoded.kind, FrameKind::Text);
        assert_eq!(decoded.data, frame);
        let reply = daemon_channel
            .encode_frame(FrameKind::Binary, &[1, 2, 3])
            .unwrap();
        assert_eq!(
            client_channel.decode_record(&reply).unwrap().data,
            vec![1, 2, 3]
        );
    }

    #[test]
    fn legacy_and_malformed_readies_are_fatal_4002() {
        let cases = [
            r#"{"type":"e2ee_ready","capabilities":{"binaryCiphertext":true}}"#.to_string(),
            r#"{"type":"e2ee_ready","protected":{"v":2,"message":"AAAA"}}"#.to_string(),
            r#"{"type":"e2ee_ready","protected":{"v":1,"message":"not base64!!"}}"#.to_string(),
            r#"{"type":"e2ee_ready","protected":{"v":1}}"#.to_string(),
            serde_json::json!({
                "type": "e2ee_ready",
                "protected": {"v": 1, "message": base64_encode(&[7u8; 50])},
            })
            .to_string(),
        ];
        for text in &cases {
            let mut client = seeded_client();
            let outcome = client.accept_ready(text);
            assert!(outcome.is_err(), "must be fatal: {text}");
            let error = outcome.err().unwrap().copy_error();
            assert_eq!(
                error.close_code, PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE,
                "{text}"
            );
        }
        // Non-ready traffic while handshaking is ignored, not fatal.
        let mut client = seeded_client();
        assert!(
            client
                .accept_ready(r#"{"type":"session"}"#)
                .unwrap()
                .is_none()
        );
        assert!(client.accept_ready("not json").unwrap().is_none());
    }

    #[test]
    fn parameter_payload_rules_mirror_the_typescript_decoder() {
        assert!(decode_params(&[0x01, 0x01]).unwrap().binary_ciphertext);
        assert!(!decode_params(&[0x01, 0x00]).unwrap().binary_ciphertext);
        for payload in [&[0x02u8, 0x00][..], &[0x01, 0x02][..], &[0x01][..], &[][..]] {
            let outcome = decode_params(payload);
            assert!(outcome.is_err(), "must fail");
            let error = outcome.err().unwrap().copy_error();
            assert_eq!(error.close_code, PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE);
        }
    }

    #[test]
    fn relay_security_requires_a_well_formed_daemon_public_key() {
        assert!(RelaySecurity::new("not base64!!".into()).is_err());
        assert!(RelaySecurity::new(base64_encode(&[0u8; 16])).is_err());
        assert!(RelaySecurity::new(DAEMON_STATIC_PUBLIC.into()).is_ok());
    }

    #[test]
    fn a_non_contributing_daemon_static_key_never_completes_a_handshake() {
        // All-zero static key: the es shared secret is all zeros, so the
        // fail-closed DH wrapper must abort the handshake before any ready.
        let all_zero = ClientHandshake::begin(&base64_encode(&[0u8; 32]));
        assert!(
            all_zero.is_err(),
            "an all-zero daemon static key must not handshake"
        );
        // A low-order X25519 point (a documented all-zero-output input) is the
        // same failure class.
        let low_order: [u8; 32] = [
            0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
            0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
            0x5f, 0x49, 0xb8, 0x00,
        ];
        assert!(
            ClientHandshake::begin(&base64_encode(&low_order)).is_err(),
            "a low-order daemon static key must not handshake"
        );
        // Positive control: a real static key still handshakes, so the wrapper
        // rejects only non-contributing DH outputs.
        let mut client = seeded_client();
        let (ready, _daemon_channel) = daemon_respond(&mut client);
        assert!(client.accept_ready(&ready).is_ok());
    }

    #[test]
    fn open_channel_classifies_plaintext_traffic_like_the_typescript_channel() {
        for text in [
            r#"{"type":"e2ee_hello","key":"x"}"#,
            r#"{"type":"e2ee_ready","protected":{"v":1,"message":"AAAA"}}"#,
        ] {
            assert!(matches!(
                classify_plaintext_text(text),
                PlaintextTraffic::HandshakeArtifact
            ));
        }
        assert!(matches!(
            classify_plaintext_text(r#"{"type":"session"}"#),
            PlaintextTraffic::PlaintextApplication
        ));
        assert!(matches!(
            classify_plaintext_text("{broken json"),
            PlaintextTraffic::PlaintextApplication
        ));
        assert!(matches!(
            classify_plaintext_text("AAAABBBBCCCC"),
            PlaintextTraffic::RecordBase64
        ));
    }

    #[test]
    fn record_errors_are_4003_not_handshake_failures() {
        let (_client, mut daemon_channel) = open_pair();
        let outcome = daemon_channel.decode_record(&[0x00, 0x10, 0xff, 0x00, 0x01, 0x02]);
        assert!(outcome.is_err(), "garbage record must fail");
        let error = outcome.err().unwrap().copy_error();
        assert_eq!(error.close_code, PROTECTED_RECORD_FAILED_CLOSE_CODE);
    }

    #[test]
    fn error_reasons_are_static_text_without_key_material() {
        let mut client = seeded_client();
        let outcome = client.accept_ready(r#"{"type":"e2ee_ready"}"#);
        assert!(outcome.is_err(), "legacy ready must fail");
        let error = outcome.err().unwrap().copy_error();
        assert!(error.reason.is_ascii());
        assert!(
            !error
                .to_string()
                .contains(&DAEMON_STATIC_PUBLIC.to_lowercase())
        );
    }

    impl TransportSecurityError {
        fn copy_error(&self) -> TransportSecurityError {
            TransportSecurityError {
                close_code: self.close_code,
                reason: self.reason,
            }
        }
    }
}

#[cfg(test)]
mod live_interop {
    use super::*;
    use crate::relay::FrameKind;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    fn daemon_public_key_b64() -> String {
        base64_encode(&[
            0x8c, 0xf6, 0x1a, 0x66, 0xf5, 0x88, 0xc7, 0xed, 0xeb, 0x1a, 0x63, 0x87, 0x89, 0x9f,
            0xa7, 0xc2, 0xf5, 0xf3, 0xac, 0x6e, 0xf5, 0x8b, 0xa0, 0xc4, 0x75, 0xc4, 0x27, 0x90,
            0xd3, 0xe1, 0xa3, 0x6c,
        ])
    }

    /// Live cross-implementation interop: connect to a real daemon-side
    /// responder running the TypeScript `openDaemonSecureChannel` from
    /// `packages/relay` (started by scripts/… bridge, URL in
    /// PASEO_RELAY_BRIDGE_URL), shake hands, and exchange text and binary
    /// records in both directions. Ignored by default so the suite never
    /// depends on a running bridge.
    #[tokio::test]
    #[ignore = "requires the live TS relay bridge: PASEO_RELAY_BRIDGE_URL=ws://…"]
    async fn interops_with_the_real_typescript_daemon_channel() {
        let Ok(url) = std::env::var("PASEO_RELAY_BRIDGE_URL") else {
            panic!("set PASEO_RELAY_BRIDGE_URL to run the live interop test");
        };
        let (mut socket, _) = connect_async(&url).await.expect("connect to the bridge");
        let mut handshake = ClientHandshake::begin(&daemon_public_key_b64()).unwrap();
        socket
            .send(Message::Text(handshake.hello_text().to_owned()))
            .await
            .unwrap();
        loop {
            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                continue;
            };
            if let Some(mut channel) = handshake.accept_ready(&text).unwrap() {
                assert!(channel.binary_ciphertext());
                let frame = b"live typescript interop frame".to_vec();
                let record = channel.encode_frame(FrameKind::Text, &frame).unwrap();
                socket
                    .send(Message::Text(base64_encode(&record)))
                    .await
                    .unwrap();
                let echo = socket.next().await.unwrap().unwrap();
                let record = match echo {
                    Message::Text(text) => base64_decode(&text).unwrap(),
                    Message::Binary(bytes) => bytes,
                    other => panic!("unexpected echo frame {other:?}"),
                };
                assert_eq!(channel.decode_record(&record).unwrap().data, frame);
                let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
                let record = channel.encode_frame(FrameKind::Binary, &big).unwrap();
                socket.send(Message::Binary(record)).await.unwrap();
                let echo = socket.next().await.unwrap().unwrap();
                let record = match echo {
                    Message::Text(text) => base64_decode(&text).unwrap(),
                    Message::Binary(bytes) => bytes,
                    other => panic!("unexpected echo frame {other:?}"),
                };
                assert_eq!(channel.decode_record(&record).unwrap().data, big);
                return;
            }
        }
    }
}

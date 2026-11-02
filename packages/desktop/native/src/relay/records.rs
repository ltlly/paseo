//! Authenticated application-frame framing over Noise transport messages.
//!
//! One application frame becomes one wire record, mirroring
//! `packages/relay/src/protected-records.ts` byte for byte:
//!
//! ```text
//! chunk plaintext = kind(1) || flags(1) || totalLength(u32 BE) || data
//! wire record     = for each chunk: ciphertextLength(u16 BE) || ciphertext
//! ```
//!
//! `kind` is `0x00` text or `0x01` binary. `flags` bit 0 marks the first chunk
//! and bit 1 the last. Transport AD is empty, as in Noise §5; framing
//! authenticity comes from the metadata being inside the ciphertext.

use base64::Engine;
use snow::TransportState;

use crate::relay::{FrameKind, TransportSecurityError};

pub(crate) const NOISE_MAX_MESSAGE_BYTES: usize = 65535;
pub(crate) const NOISE_TAG_BYTES: usize = 16;
const KIND_TEXT: u8 = 0x00;
const KIND_BINARY: u8 = 0x01;
const FLAG_FIRST: u8 = 0x01;
const FLAG_FINAL: u8 = 0x02;
const HEADER_BYTES: usize = 6;
const LENGTH_PREFIX_BYTES: usize = 2;
/// Largest data payload a single chunk carries (a chunk never exceeds the
/// Noise message limit).
pub(crate) const MAX_CHUNK_DATA_BYTES: usize =
    NOISE_MAX_MESSAGE_BYTES - NOISE_TAG_BYTES - HEADER_BYTES;
const CHUNK_OVERHEAD_BYTES: usize = LENGTH_PREFIX_BYTES + HEADER_BYTES + NOISE_TAG_BYTES;

pub(crate) fn chunk_count(frame_bytes: usize) -> usize {
    frame_bytes.div_ceil(MAX_CHUNK_DATA_BYTES).max(1)
}

/// Wire bytes a frame of `frame_bytes` occupies once protected.
pub(crate) fn protected_record_byte_length(frame_bytes: usize) -> usize {
    frame_bytes + chunk_count(frame_bytes) * CHUNK_OVERHEAD_BYTES
}

/// Largest application frame whose protected record still fits one relay
/// WebSocket message once base64-encoded, mirroring
/// `maxSecuredPlaintextByteLength` in `packages/relay/src/secure-channel.ts`
/// (the tighter text bound is enforced for both kinds).
pub fn max_relay_frame_bytes(max_ws_bytes: usize) -> usize {
    let base64_capacity = max_ws_bytes / 4 * 3;
    let mut bound = base64_capacity;
    while bound > 0 && protected_record_byte_length(bound) > base64_capacity {
        let overshoot = protected_record_byte_length(bound) - bound;
        bound = base64_capacity.saturating_sub(overshoot);
    }
    bound
}

/// One decoded application frame; `data` is authenticated plaintext.
#[derive(Debug)]
pub struct DecodedFrame {
    pub kind: FrameKind,
    pub data: Vec<u8>,
}

/// Encodes one application frame as a single record. The nonce for every chunk
/// is reserved synchronously inside `transport`; no await can interleave.
pub(crate) fn encode_record(
    kind: FrameKind,
    frame: &[u8],
    transport: &mut TransportState,
) -> Result<Vec<u8>, TransportSecurityError> {
    let chunks = chunk_count(frame.len());
    let mut out = Vec::with_capacity(protected_record_byte_length(frame.len()));
    let mut ciphertext = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
    for index in 0..chunks {
        let start = index * MAX_CHUNK_DATA_BYTES;
        let end = (start + MAX_CHUNK_DATA_BYTES).min(frame.len());
        let data = &frame[start..end];
        let mut plaintext = Vec::with_capacity(HEADER_BYTES + data.len());
        plaintext.push(match kind {
            FrameKind::Text => KIND_TEXT,
            FrameKind::Binary => KIND_BINARY,
        });
        plaintext
            .push(u8::from(index == 0) * FLAG_FIRST | u8::from(index + 1 == chunks) * FLAG_FINAL);
        plaintext.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        plaintext.extend_from_slice(data);
        let written = transport
            .write_message(&plaintext, &mut ciphertext)
            .map_err(|_| TransportSecurityError::record("record protection failed"))?;
        out.extend_from_slice(&(written as u16).to_be_bytes());
        out.extend_from_slice(&ciphertext[..written]);
    }
    Ok(out)
}

/// Decodes one wire record. Chunks are authenticated in order and the receive
/// counter advances per authenticated chunk; any framing violation errors and
/// nothing partial is returned. The caller must close the channel on error.
pub(crate) fn decode_record(
    record: &[u8],
    transport: &mut TransportState,
) -> Result<DecodedFrame, TransportSecurityError> {
    let mut offset = 0;
    let mut assembly: Option<Assembly> = None;
    let mut scratch = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
    while offset < record.len() {
        if assembly
            .as_ref()
            .is_some_and(|a| a.index >= a.expected_chunks)
        {
            return Err(TransportSecurityError::record(
                "unexpected chunk after final",
            ));
        }
        let chunk = read_chunk(record, &mut offset, transport, &mut scratch)?;
        assembly = match assembly {
            None => Some(start_assembly(chunk, record.len())?),
            Some(existing) => Some(continue_assembly(existing, chunk)?),
        };
    }
    let Some(assembly) = assembly else {
        return Err(TransportSecurityError::record("empty record"));
    };
    if assembly.index != assembly.expected_chunks || assembly.written != assembly.total_length {
        return Err(TransportSecurityError::record("record incomplete"));
    }
    Ok(DecodedFrame {
        kind: assembly.kind,
        data: assembly.frame,
    })
}

struct Chunk {
    kind: FrameKind,
    is_first: bool,
    is_final: bool,
    total_length: usize,
    data: Vec<u8>,
}

struct Assembly {
    kind: FrameKind,
    total_length: usize,
    expected_chunks: usize,
    frame: Vec<u8>,
    written: usize,
    index: usize,
}

fn read_chunk(
    record: &[u8],
    offset: &mut usize,
    transport: &mut TransportState,
    scratch: &mut [u8],
) -> Result<Chunk, TransportSecurityError> {
    if record.len() - *offset < LENGTH_PREFIX_BYTES {
        return Err(TransportSecurityError::record("record truncated"));
    }
    let ciphertext_length = u16::from_be_bytes([record[*offset], record[*offset + 1]]) as usize;
    let start = *offset + LENGTH_PREFIX_BYTES;
    if !(NOISE_TAG_BYTES + HEADER_BYTES..=NOISE_MAX_MESSAGE_BYTES).contains(&ciphertext_length) {
        return Err(TransportSecurityError::record("invalid chunk length"));
    }
    if record.len() - start < ciphertext_length {
        return Err(TransportSecurityError::record("record truncated"));
    }
    let written = transport
        .read_message(&record[start..start + ciphertext_length], scratch)
        .map_err(|_| TransportSecurityError::record("record authentication failed"))?;
    *offset = start + ciphertext_length;
    let plaintext = &scratch[..written];
    if plaintext.len() < HEADER_BYTES {
        return Err(TransportSecurityError::record("chunk header truncated"));
    }
    if plaintext[1] & !(FLAG_FIRST | FLAG_FINAL) != 0 {
        return Err(TransportSecurityError::record("reserved flag set"));
    }
    Ok(Chunk {
        kind: decode_kind(plaintext[0])?,
        is_first: plaintext[1] & FLAG_FIRST != 0,
        is_final: plaintext[1] & FLAG_FINAL != 0,
        total_length: u32::from_be_bytes(plaintext[2..6].try_into().expect("u32 header")) as usize,
        data: plaintext[HEADER_BYTES..].to_vec(),
    })
}

fn start_assembly(chunk: Chunk, record_len: usize) -> Result<Assembly, TransportSecurityError> {
    if !chunk.is_first {
        return Err(TransportSecurityError::record(
            "record does not start with FIRST",
        ));
    }
    // The claimed frame length fixes the record's exact wire length. Check it
    // before allocating so a tiny authenticated header cannot demand gigabytes.
    if protected_record_byte_length(chunk.total_length) != record_len {
        return Err(TransportSecurityError::record("record length mismatch"));
    }
    let mut assembly = Assembly {
        kind: chunk.kind,
        total_length: chunk.total_length,
        expected_chunks: chunk_count(chunk.total_length),
        frame: vec![0u8; chunk.total_length],
        written: 0,
        index: 0,
    };
    append_chunk(&mut assembly, &chunk)?;
    Ok(assembly)
}

fn continue_assembly(
    mut assembly: Assembly,
    chunk: Chunk,
) -> Result<Assembly, TransportSecurityError> {
    if chunk.is_first {
        return Err(TransportSecurityError::record(
            "FIRST repeated inside record",
        ));
    }
    if chunk.kind != assembly.kind {
        return Err(TransportSecurityError::record(
            "frame kind changed inside record",
        ));
    }
    if chunk.total_length != assembly.total_length {
        return Err(TransportSecurityError::record(
            "frame length changed inside record",
        ));
    }
    append_chunk(&mut assembly, &chunk)?;
    Ok(assembly)
}

fn append_chunk(assembly: &mut Assembly, chunk: &Chunk) -> Result<(), TransportSecurityError> {
    let is_last_expected = assembly.index == assembly.expected_chunks - 1;
    if chunk.is_final != is_last_expected {
        return Err(TransportSecurityError::record("FINAL flag mismatch"));
    }
    let expected_data = if is_last_expected {
        assembly.total_length - assembly.index * MAX_CHUNK_DATA_BYTES
    } else {
        MAX_CHUNK_DATA_BYTES
    };
    if chunk.data.len() != expected_data {
        return Err(TransportSecurityError::record("chunk size mismatch"));
    }
    assembly.frame[assembly.written..assembly.written + chunk.data.len()]
        .copy_from_slice(&chunk.data);
    assembly.written += chunk.data.len();
    assembly.index += 1;
    Ok(())
}

fn decode_kind(byte: u8) -> Result<FrameKind, TransportSecurityError> {
    match byte {
        KIND_TEXT => Ok(FrameKind::Text),
        KIND_BINARY => Ok(FrameKind::Binary),
        _ => Err(TransportSecurityError::record("invalid frame kind")),
    }
}

/// Base64 (standard alphabet, padded), exactly as the TypeScript endpoints
/// exchange records in text WebSocket frames.
pub(crate) fn base64_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub(crate) fn base64_decode(text: &str) -> Result<Vec<u8>, TransportSecurityError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| TransportSecurityError::record("invalid record encoding"))
}

#[allow(clippy::needless_range_loop)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{
        MAX_RELAY_WEBSOCKET_MESSAGE_BYTES, NOISE_PROTOCOL_NAME, PROTECTED_RECORD_FAILED_CLOSE_CODE,
    };

    // Deterministic pair of transport states for framing tests.
    fn pair() -> (TransportState, TransportState) {
        let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME.parse().unwrap();
        let builder = snow::Builder::new(params.clone());
        let responder_static = builder.generate_keypair().unwrap();
        let responder = snow::Builder::new(params.clone())
            .local_private_key(&responder_static.private)
            .expect("set the responder static key")
            .build_responder()
            .unwrap();
        let initiator = snow::Builder::new(params)
            .remote_public_key(&responder_static.public)
            .expect("set the initiator remote static key")
            .build_initiator()
            .unwrap();
        let mut initiator = initiator;
        let mut responder = responder;
        let mut buffer = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
        let written = initiator.write_message(&[0x01, 0x01], &mut buffer).unwrap();
        let message = buffer[..written].to_vec();
        responder.read_message(&message, &mut buffer).unwrap();
        let written = responder.write_message(&[0x01, 0x01], &mut buffer).unwrap();
        let message = buffer[..written].to_vec();
        initiator.read_message(&message, &mut buffer).unwrap();
        (
            initiator.into_transport_mode().unwrap(),
            responder.into_transport_mode().unwrap(),
        )
    }

    #[test]
    fn records_round_trip_across_chunk_boundaries() {
        let (mut client, mut daemon) = pair();
        for frame in [
            vec![],
            b"hello protected relay".to_vec(),
            vec![7u8; MAX_CHUNK_DATA_BYTES],
            vec![9u8; MAX_CHUNK_DATA_BYTES + 1],
            (0..200_000u32).map(|i| (i % 251) as u8).collect(),
        ] {
            let record = encode_record(FrameKind::Binary, &frame, &mut client).unwrap();
            let decoded = decode_record(&record, &mut daemon).unwrap();
            assert_eq!(decoded.kind, FrameKind::Binary);
            assert_eq!(decoded.data, frame);
        }
    }

    #[test]
    fn tampered_or_replayed_or_reflected_records_never_deliver() {
        let (mut client, mut daemon) = pair();
        let frame = b"tamper target".to_vec();
        let record = encode_record(FrameKind::Text, &frame, &mut client).unwrap();

        // Tampered ciphertext.
        let mut tampered = record.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(decode_record(&tampered, &mut daemon).is_err());

        // Replayed record: the exact wire bytes that already decoded once must
        // never decode again — the receive nonce advanced after the first.
        assert!(decode_record(&record, &mut daemon).is_ok());
        assert!(decode_record(&record, &mut daemon).is_err());

        // Reflected record: the same transport reading its own send output on
        // its receive direction (the split directions are independent keys).
        let (mut sender, _unused) = pair();
        let own = encode_record(FrameKind::Text, &frame, &mut sender).unwrap();
        assert!(decode_record(&own, &mut sender).is_err());

        // Reordered: chunk 2 of a two-chunk record delivered alone as a record
        // (a relay cannot merge or split records without authentication failing
        // or the framing rules rejecting it).
        let two_chunk = vec![1u8; MAX_CHUNK_DATA_BYTES + 3];
        let (mut client, mut daemon) = pair();
        let big = encode_record(FrameKind::Binary, &two_chunk, &mut client).unwrap();
        let first_chunk_len = u16::from_be_bytes([big[0], big[1]]) as usize + 2;
        let second = big[first_chunk_len..].to_vec();
        assert!(decode_record(&second, &mut daemon).is_err());
    }

    #[test]
    fn authenticated_malformed_headers_reject_with_their_framing_reason() {
        // One sender/receiver pair: the malformed headers below carry valid
        // authentication tags, so the rejection is the framing rule itself —
        // never an accidental tag failure from a mismatched handshake.
        let (mut sender, mut receiver) = pair();
        let mut out = vec![0u8; NOISE_MAX_MESSAGE_BYTES];
        let mut record = |plaintext: &[u8]| -> Vec<u8> {
            let written = sender.write_message(plaintext, &mut out).unwrap();
            let mut wire = (written as u16).to_be_bytes().to_vec();
            wire.extend_from_slice(&out[..written]);
            wire
        };

        // Reserved flag bit set inside an authenticated chunk.
        let reserved_flag = record(&[KIND_BINARY, 0b100, 0, 0, 0, 1, 0xAA]);
        let error = decode_record(&reserved_flag, &mut receiver).unwrap_err();
        assert_eq!(error.close_code, PROTECTED_RECORD_FAILED_CLOSE_CODE);
        assert_eq!(error.reason, "reserved flag set");

        // Invalid frame kind.
        let invalid_kind = record(&[0x02, FLAG_FIRST | FLAG_FINAL, 0, 0, 0, 1, 0xAA]);
        let error = decode_record(&invalid_kind, &mut receiver).unwrap_err();
        assert_eq!(error.reason, "invalid frame kind");

        // An authenticated header claiming a total length that disagrees with
        // the record's actual wire length.
        let mut header = vec![KIND_TEXT, FLAG_FIRST | FLAG_FINAL];
        header.extend_from_slice(&9u32.to_be_bytes());
        header.push(0xAA);
        let mismatched = record(&header);
        let error = decode_record(&mismatched, &mut receiver).unwrap_err();
        assert_eq!(error.reason, "record length mismatch");

        // Truncated record.
        let frame = b"truncated".to_vec();
        let whole = encode_record(FrameKind::Text, &frame, &mut sender).unwrap();
        assert!(decode_record(&whole[..whole.len() - 4], &mut receiver).is_err());
        // Empty record.
        assert!(decode_record(&[], &mut receiver).is_err());
    }

    #[test]
    fn max_relay_frame_bytes_keeps_the_base64_record_within_the_ws_limit() {
        let limit = max_relay_frame_bytes(MAX_RELAY_WEBSOCKET_MESSAGE_BYTES);
        // The returned bound always fits, and is within one chunk overhead of
        // the exact maximum (the algorithm steps down by the whole overshoot,
        // mirroring `maxSecuredPlaintextByteLength` in secure-channel.ts).
        let record = protected_record_byte_length(limit);
        assert!(record.div_ceil(3) * 4 <= MAX_RELAY_WEBSOCKET_MESSAGE_BYTES);
        let mut exact = MAX_RELAY_WEBSOCKET_MESSAGE_BYTES / 4 * 3;
        while protected_record_byte_length(exact) > MAX_RELAY_WEBSOCKET_MESSAGE_BYTES / 4 * 3 {
            exact -= 1;
        }
        assert!(limit + CHUNK_OVERHEAD_BYTES >= exact);
        for frame in [
            0usize,
            1,
            MAX_CHUNK_DATA_BYTES,
            MAX_CHUNK_DATA_BYTES + 1,
            limit,
        ] {
            let wire = protected_record_byte_length(frame);
            assert!(wire.div_ceil(3) * 4 <= MAX_RELAY_WEBSOCKET_MESSAGE_BYTES);
        }
    }

    #[test]
    fn chunk_count_never_collapses_to_zero() {
        assert_eq!(chunk_count(0), 1);
        assert_eq!(chunk_count(1), 1);
        assert_eq!(chunk_count(MAX_CHUNK_DATA_BYTES), 1);
        assert_eq!(chunk_count(MAX_CHUNK_DATA_BYTES + 1), 2);
    }
}

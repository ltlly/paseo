use anyhow::{Result, bail, ensure};
use serde::Deserialize;

pub const MAX_MESSAGE_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Default)]
pub struct ChunkDecoder {
    assembly: Option<(u8, Vec<u8>)>,
}

impl ChunkDecoder {
    pub fn receive(&mut self, frame: Frame) -> Result<Option<Frame>> {
        match frame {
            Frame::Binary(bytes) if bytes.first() == Some(&0x31) => {
                ensure!(bytes.len() > 4, "empty chunk frame");
                ensure!(
                    bytes[1] == 1 && bytes[2] <= 1 && bytes[3] & !3 == 0,
                    "invalid chunk header"
                );
                let start = bytes[3] & 1 != 0;
                let end = bytes[3] & 2 != 0;
                ensure!(start != self.assembly.is_some(), "invalid chunk sequence");
                if start {
                    self.assembly = Some((bytes[2], Vec::new()));
                }
                let (kind, assembled) = self.assembly.as_mut().expect("validated chunk start");
                ensure!(*kind == bytes[2], "chunk kind changed");
                ensure!(
                    assembled.len() + bytes.len() - 4 <= MAX_MESSAGE_BYTES,
                    "logical message too large"
                );
                assembled.extend_from_slice(&bytes[4..]);
                if !end {
                    return Ok(None);
                }
                let (kind, assembled) = self.assembly.take().expect("validated chunk end");
                Ok(Some(if kind == 0 {
                    Frame::Text(String::from_utf8(assembled)?)
                } else {
                    Frame::Binary(assembled)
                }))
            }
            frame => {
                ensure!(self.assembly.is_none(), "interrupted chunked message");
                let size = match &frame {
                    Frame::Text(s) => s.len(),
                    Frame::Binary(b) => b.len(),
                };
                ensure!(size <= MAX_MESSAGE_BYTES, "logical message too large");
                Ok(Some(frame))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub enum TerminalOpcode {
    Output = 1,
    Input = 2,
    Resize = 3,
    Snapshot = 4,
    Restore = 5,
    ReplayBegin = 6,
    ReplayData = 7,
    ReplayEnd = 8,
}

#[derive(Debug, PartialEq)]
pub struct TerminalFrame {
    pub opcode: TerminalOpcode,
    pub slot: u8,
    pub data: Vec<u8>,
}

impl TerminalFrame {
    pub fn decode(bytes: Vec<u8>) -> Result<Self> {
        ensure!(bytes.len() >= 2, "truncated terminal frame");
        let opcode = match bytes[0] {
            1 => TerminalOpcode::Output,
            2 => TerminalOpcode::Input,
            3 => TerminalOpcode::Resize,
            4 => TerminalOpcode::Snapshot,
            5 => TerminalOpcode::Restore,
            6 => TerminalOpcode::ReplayBegin,
            7 => TerminalOpcode::ReplayData,
            8 => TerminalOpcode::ReplayEnd,
            _ => bail!("unknown terminal opcode"),
        };
        Ok(Self {
            opcode,
            slot: bytes[1],
            data: bytes[2..].to_vec(),
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(2 + self.data.len());
        bytes.extend_from_slice(&[self.opcode as u8, self.slot]);
        bytes.extend_from_slice(&self.data);
        bytes
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayMetadata {
    pub rows: u16,
    pub cols: u16,
    pub retained_history_lines: usize,
}

impl ReplayMetadata {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes)?;
        ensure!(value.rows > 0 && value.cols > 0, "invalid replay geometry");
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_text_preserves_segmented_utf8() {
        let mut decoder = ChunkDecoder::default();
        assert_eq!(
            decoder
                .receive(Frame::Binary(vec![0x31, 1, 0, 1, 0xe4]))
                .unwrap(),
            None
        );
        assert_eq!(
            decoder
                .receive(Frame::Binary(vec![0x31, 1, 0, 0, 0xb8]))
                .unwrap(),
            None
        );
        assert_eq!(
            decoder
                .receive(Frame::Binary(vec![0x31, 1, 0, 2, 0xad]))
                .unwrap(),
            Some(Frame::Text("中".into()))
        );
    }

    #[test]
    fn invalid_sequences_fail_without_delivering_partial_messages() {
        for bytes in [
            vec![0x31, 1, 0, 2, 1],
            vec![0x31, 2, 0, 3, 1],
            vec![0x31, 1, 0, 4, 1],
        ] {
            assert!(
                ChunkDecoder::default()
                    .receive(Frame::Binary(bytes))
                    .is_err()
            );
        }
        let mut decoder = ChunkDecoder::default();
        decoder
            .receive(Frame::Binary(vec![0x31, 1, 1, 1, 1]))
            .unwrap();
        assert!(decoder.receive(Frame::Text("interleaved".into())).is_err());
        let mut decoder = ChunkDecoder::default();
        decoder
            .receive(Frame::Binary(vec![0x31, 1, 1, 1, 1]))
            .unwrap();
        assert!(
            decoder
                .receive(Frame::Binary(vec![0x31, 1, 0, 2, 1]))
                .is_err()
        );
    }

    #[test]
    fn terminal_payload_stays_binary_including_invalid_utf8() {
        let bytes = vec![7, 255, 0xf0, 0x9f, 0];
        let frame = TerminalFrame::decode(bytes.clone()).unwrap();
        assert_eq!(frame.encode(), bytes);
        assert_eq!(frame.slot, 255);
        assert_eq!(frame.opcode, TerminalOpcode::ReplayData);
    }

    #[test]
    fn replay_metadata_accepts_new_fields_but_rejects_invalid_geometry() {
        assert_eq!(
            ReplayMetadata::decode(
                br#"{"rows":24,"cols":80,"retainedHistoryLines":10,"future":true}"#
            )
            .unwrap()
            .cols,
            80
        );
        assert!(
            ReplayMetadata::decode(br#"{"rows":0,"cols":80,"retainedHistoryLines":10}"#).is_err()
        );
    }
}

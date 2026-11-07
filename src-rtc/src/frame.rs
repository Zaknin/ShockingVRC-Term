//! Bounded transport framing for unchanged Remote API JSON envelopes.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_json_frame_fragments_and_reassembles() {
        let frame = format!(r#"{{"type":"snapshot","payload":"{}"}}"#, "x".repeat(250_000));
        let chunks = encode_frame(frame.as_bytes()).expect("encode");
        assert!(chunks.len() > 1);

        let mut decoder = FrameDecoder::default();
        let decoded = chunks
            .iter()
            .filter_map(|chunk| decoder.push(chunk).expect("valid chunk"))
            .next()
            .expect("complete frame");
        assert_eq!(decoded, frame.as_bytes());
    }

    #[test]
    fn rejects_out_of_order_or_inconsistent_fragments() {
        let chunks = encode_frame(br#"{"type":"ping"}"#).expect("encode");
        let mut broken = chunks[0].clone();
        broken[16..18].copy_from_slice(&1_u16.to_be_bytes());
        assert!(FrameDecoder::default().push(&broken).is_err());
    }
}

pub fn encode_frame(bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
    if bytes.len() > MAX_REMOTE_FRAME_LEN {
        return Err(FrameError::TooLarge);
    }
    let count = bytes.len().max(1).div_ceil(CHUNK_DATA_LEN);
    let count = u16::try_from(count).map_err(|_| FrameError::TooLarge)?;
    let total_len = u32::try_from(bytes.len()).map_err(|_| FrameError::TooLarge)?;
    let message_id = rand_message_id();
    let mut chunks = Vec::with_capacity(usize::from(count));
    for index in 0..count {
        let start = usize::from(index) * CHUNK_DATA_LEN;
        let end = (start + CHUNK_DATA_LEN).min(bytes.len());
        let payload = bytes.get(start..end).ok_or(FrameError::Invalid)?;
        let mut chunk = Vec::with_capacity(HEADER_LEN + payload.len());
        chunk.extend_from_slice(MAGIC);
        chunk.extend_from_slice(&message_id.to_be_bytes());
        chunk.extend_from_slice(&total_len.to_be_bytes());
        chunk.extend_from_slice(&index.to_be_bytes());
        chunk.extend_from_slice(&count.to_be_bytes());
        chunk.extend_from_slice(payload);
        chunks.push(chunk);
    }
    Ok(chunks)
}

fn rand_message_id() -> u64 {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0_u8; 8];
    if SystemRandom::new().fill(&mut bytes).is_ok() {
        u64::from_be_bytes(bytes)
    } else {
        // A process-local counter is sufficient for framing correlation because the
        // DataChannel is ordered/reliable and each sender serializes writes.
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Default)]
pub struct FrameDecoder {
    assembly: Option<Assembly>,
}

struct Assembly {
    message_id: u64,
    total_len: usize,
    chunk_count: u16,
    next_chunk: u16,
    bytes: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
        if chunk.len() < HEADER_LEN || chunk.get(..4) != Some(MAGIC) {
            return Err(FrameError::Invalid);
        }
        let message_id = read_u64(chunk, 4)?;
        let total_len = usize::try_from(read_u32(chunk, 12)?).map_err(|_| FrameError::Invalid)?;
        let chunk_index = read_u16(chunk, 16)?;
        let chunk_count = read_u16(chunk, 18)?;
        if total_len > MAX_REMOTE_FRAME_LEN || chunk_count == 0 || chunk_index >= chunk_count {
            self.assembly = None;
            return Err(if total_len > MAX_REMOTE_FRAME_LEN { FrameError::TooLarge } else { FrameError::Invalid });
        }
        let expected_count = total_len.max(1).div_ceil(CHUNK_DATA_LEN);
        if expected_count != usize::from(chunk_count) {
            self.assembly = None;
            return Err(FrameError::Invalid);
        }
        let start = usize::from(chunk_index) * CHUNK_DATA_LEN;
        let expected_payload_len = total_len.saturating_sub(start).min(CHUNK_DATA_LEN);
        if chunk.len() - HEADER_LEN != expected_payload_len {
            self.assembly = None;
            return Err(FrameError::Invalid);
        }

        if chunk_index == 0 {
            if self.assembly.is_some() {
                self.assembly = None;
                return Err(FrameError::Invalid);
            }
            let mut bytes = Vec::with_capacity(total_len);
            bytes.extend_from_slice(&chunk[HEADER_LEN..]);
            if chunk_count == 1 {
                return Ok(Some(bytes));
            }
            self.assembly = Some(Assembly {
                message_id,
                total_len,
                chunk_count,
                next_chunk: 1,
                bytes,
            });
            return Ok(None);
        }

        let Some(assembly) = self.assembly.as_mut() else {
            return Err(FrameError::Invalid);
        };
        if assembly.message_id != message_id
            || assembly.total_len != total_len
            || assembly.chunk_count != chunk_count
            || assembly.next_chunk != chunk_index
        {
            self.assembly = None;
            return Err(FrameError::Invalid);
        }
        assembly.bytes.extend_from_slice(&chunk[HEADER_LEN..]);
        assembly.next_chunk = assembly.next_chunk.saturating_add(1);
        if chunk_index + 1 == chunk_count {
            let assembly = self.assembly.take().ok_or(FrameError::Invalid)?;
            if assembly.bytes.len() != assembly.total_len {
                return Err(FrameError::Invalid);
            }
            Ok(Some(assembly.bytes))
        } else {
            Ok(None)
        }
    }
}

fn read_u16(bytes: &[u8], start: usize) -> Result<u16, FrameError> {
    let array: [u8; 2] = bytes.get(start..start + 2).ok_or(FrameError::Invalid)?.try_into().map_err(|_| FrameError::Invalid)?;
    Ok(u16::from_be_bytes(array))
}

fn read_u32(bytes: &[u8], start: usize) -> Result<u32, FrameError> {
    let array: [u8; 4] = bytes.get(start..start + 4).ok_or(FrameError::Invalid)?.try_into().map_err(|_| FrameError::Invalid)?;
    Ok(u32::from_be_bytes(array))
}

fn read_u64(bytes: &[u8], start: usize) -> Result<u64, FrameError> {
    let array: [u8; 8] = bytes.get(start..start + 8).ok_or(FrameError::Invalid)?.try_into().map_err(|_| FrameError::Invalid)?;
    Ok(u64::from_be_bytes(array))
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("invalid DataChannel frame")]
    Invalid,
    #[error("Remote API frame exceeds the v1 size limit")]
    TooLarge,
}
const MAGIC: &[u8; 4] = b"SVRF";
const HEADER_LEN: usize = 20;
const DATA_CHANNEL_MESSAGE_LIMIT: usize = 16 * 1024;
const CHUNK_DATA_LEN: usize = DATA_CHANNEL_MESSAGE_LIMIT - HEADER_LEN;
const MAX_REMOTE_FRAME_LEN: usize = 256 * 1024;

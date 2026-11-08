//! herdr-core's framed mode: `[channel u32 LE][kind u8][len u32 LE][payload]`.
//!
//! Channel 0 carries JSON-RPC both ways, and every other channel is one
//! stream that a call opened.

use std::io::{self, Read};

pub(crate) const HEADER: usize = 9;
pub(crate) const MAX_PAYLOAD: usize = 16 << 20;

/// Channel 0: one JSON-RPC message. Other channels: stream bytes.
pub(crate) const DATA: u8 = 0;
/// A u32 LE count of bytes the receiver consumed.
pub(crate) const CREDIT: u8 = 1;
/// The channel is over. From the core, a JSON reason.
pub(crate) const CLOSE: u8 = 2;
/// A u16 LE column count, then a u16 LE row count.
pub(crate) const RESIZE: u8 = 3;
/// One byte: 1 when the viewer gained focus, 0 when it lost it.
pub(crate) const FOCUS: u8 = 4;
/// The viewer's display facts as JSON.
pub(crate) const FACTS: u8 = 5;
/// Bytes the terminal generated to answer a query, not typing.
pub(crate) const AUTOMATIC_INPUT: u8 = 6;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) channel: u32,
    pub(crate) kind: u8,
    pub(crate) payload: Vec<u8>,
}

pub(crate) fn encode(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&channel.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// The next frame, or `None` when the stream ends between frames.
pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut header = [0u8; HEADER];
    match reader.read_exact(&mut header) {
        Ok(()) => {},
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let channel = u32::from_le_bytes(header[0..4].try_into().expect("four bytes"));
    let len = u32::from_le_bytes(header[5..9].try_into().expect("four bytes")) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a {len} byte frame")));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    Ok(Some(Frame { channel, kind: header[4], payload }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_as_it_was_written() {
        let mut bytes = encode(7, RESIZE, &[80, 0, 24, 0]);
        bytes.extend(encode(0, DATA, b"{}"));
        let mut reader = bytes.as_slice();

        assert_eq!(
            read_frame(&mut reader).unwrap(),
            Some(Frame { channel: 7, kind: RESIZE, payload: vec![80, 0, 24, 0] })
        );
        assert_eq!(
            read_frame(&mut reader).unwrap(),
            Some(Frame { channel: 0, kind: DATA, payload: b"{}".to_vec() })
        );
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_frame_cut_inside_its_payload_is_an_error() {
        let bytes = encode(1, DATA, b"hello");
        assert!(read_frame(&mut &bytes[..bytes.len() - 1]).is_err());
    }
}

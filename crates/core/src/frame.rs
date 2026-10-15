//! Length-prefixed frames (a 4-byte length) on a plain byte stream: they
//! carry pairing, before any key is trusted. Everything after runs in the
//! TLS channel ([`crate::channel`]).

use std::io::{self, Read, Write};

/// The largest plain frame: pairing messages are a few hundred bytes.
pub const MAX_PLAIN_FRAME: usize = 64 * 1024;

pub fn write_frame(w: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(data)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r, 100).unwrap(), b"hello");
        assert_eq!(read_frame(&mut r, 100).unwrap(), b"");
    }

    #[test]
    fn oversized_frame_is_refused() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &[0u8; 10]).unwrap();
        assert!(read_frame(&mut &buf[..], 5).is_err());
    }
}

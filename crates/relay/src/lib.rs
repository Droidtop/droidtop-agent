//! A droidtop-run relay, DERP-style (docs/DESIGN.md section 10, transport 4; Droidtop/tracker#800):
//! the fallback when two paired devices cannot reach each other directly, not even with a punched
//! hole. It forwards WireGuard packets between two public keys and nothing else.
//!
//! **What the relay sees.** The packets are the tunnel's own (lib-sync's `tunnel`), so they are
//! already sealed by WireGuard with keys from the two devices' pairing identities before they leave
//! a device, and the session channel's TLS runs inside them. The relay learns the two public keys,
//! the packets' sizes and times, never a key that opens them and never plaintext. A packet for a key
//! is forwarded only when that key's device said it takes packets from the sender ([`Frame::Allow`]),
//! so the relay is not an open forwarder.
//!
//! **How a tunnel goes through it.** The tunnel speaks UDP, so a [`Bridge`] stands in for the other
//! side's address: a loopback UDP socket per call. Whatever the tunnel sends to it goes to the relay
//! for the peer; whatever the relay brings back comes out of it. Neither the tunnel nor the session
//! channel changes, and each call keeps its own session (Droidtop/tracker#529) because each has its
//! own flow and its own loopback socket on both sides.
//!
//! **The wire.** TCP to the relay, then frames: a type byte, a 4-byte big-endian length, the body.
//! The relay sends [`Frame::Challenge`]; the device answers [`Frame::Hello`] with its public key and
//! its signature of the challenge, and then sends [`Frame::Allow`] and [`Frame::Send`] as it needs.

mod bridge;
mod server;

pub use bridge::Bridge;
pub use server::{serve, Relay};

use std::io::{self, Read, Write};

use droidtop_agent_core::keys::PeerId;

/// The relay's TCP port, beside the agent's own (47610) and WireGuard's (47611).
pub const RELAY_PORT: u16 = 47_615;

/// What a device signs to show the relay it holds its key: this, then the relay's challenge.
pub const HELLO_CONTEXT: &[u8] = b"droidtop-agent relay v1 hello";

/// The largest packet the relay forwards: a WireGuard packet of the tunnel's largest datagram.
pub const MAX_PACKET: usize = 65_536 + 256;

/// The most keys one device may allow.
pub const MAX_ALLOWED: usize = 1024;

/// One frame between a device and the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Relay to device: 32 random bytes to sign.
    Challenge([u8; 32]),
    /// Device to relay: its key and its signature of [`HELLO_CONTEXT`] and the challenge.
    Hello { key: PeerId, signature: [u8; 64] },
    /// Device to relay: the keys it takes packets from, replacing the last list.
    Allow(Vec<PeerId>),
    /// Device to relay: a packet for [`to`] on one of their calls.
    Send { to: PeerId, flow: u32, packet: Vec<u8> },
    /// Relay to device: a packet from [`from`] on one of their calls.
    Recv { from: PeerId, flow: u32, packet: Vec<u8> },
}

const CHALLENGE: u8 = 1;
const HELLO: u8 = 2;
const ALLOW: u8 = 3;
const SEND: u8 = 4;
const RECV: u8 = 5;

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn id(bytes: &[u8]) -> PeerId {
    PeerId(bytes.try_into().expect("32 bytes"))
}

impl Frame {
    /// The frame as it goes on the wire.
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Frame::Challenge(c) => (CHALLENGE, c.to_vec()),
            Frame::Hello { key, signature } => (HELLO, [&key.0[..], &signature[..]].concat()),
            Frame::Allow(keys) => (ALLOW, keys.iter().flat_map(|k| k.0).collect()),
            Frame::Send { to, flow, packet } => (SEND, [&to.0[..], &flow.to_be_bytes(), packet].concat()),
            Frame::Recv { from, flow, packet } => (RECV, [&from.0[..], &flow.to_be_bytes(), packet].concat()),
        };
        let mut out = Vec::with_capacity(5 + body.len());
        out.push(kind);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Writes the frame to [`w`] in one write.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&self.encode())
    }

    /// The next frame from [`r`]. A frame larger than any the protocol makes is refused before it is
    /// read, so a peer cannot make the relay hold more than one packet's worth.
    pub fn read_from(r: &mut impl Read) -> io::Result<Frame> {
        let mut head = [0u8; 5];
        r.read_exact(&mut head)?;
        let len = u32::from_be_bytes(head[1..5].try_into().expect("4 bytes")) as usize;
        let limit = match head[0] {
            CHALLENGE => 32,
            HELLO => 96,
            ALLOW => 32 * MAX_ALLOWED,
            SEND | RECV => 36 + MAX_PACKET,
            _ => return Err(bad("not a relay frame")),
        };
        if len > limit {
            return Err(bad("a relay frame is too large"));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)?;
        Ok(match head[0] {
            CHALLENGE if len == 32 => Frame::Challenge(body.try_into().expect("32 bytes")),
            HELLO if len == 96 => Frame::Hello { key: id(&body[..32]), signature: body[32..].try_into().expect("64 bytes") },
            ALLOW if len % 32 == 0 => Frame::Allow(body.chunks(32).map(id).collect()),
            SEND if len >= 36 => Frame::Send {
                to: id(&body[..32]),
                flow: u32::from_be_bytes(body[32..36].try_into().expect("4")),
                packet: body[36..].to_vec(),
            },
            RECV if len >= 36 => Frame::Recv {
                from: id(&body[..32]),
                flow: u32::from_be_bytes(body[32..36].try_into().expect("4")),
                packet: body[36..].to_vec(),
            },
            _ => return Err(bad("a relay frame of the wrong size")),
        })
    }
}

/// What a device signs for [`challenge`].
pub fn hello_message(challenge: &[u8; 32]) -> Vec<u8> {
    [HELLO_CONTEXT, &challenge[..]].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_oversized_ones_are_refused() {
        let k = PeerId([7; 32]);
        for f in [
            Frame::Challenge([1; 32]),
            Frame::Hello { key: k, signature: [2; 64] },
            Frame::Allow(vec![k, PeerId([8; 32])]),
            Frame::Send { to: k, flow: 9, packet: b"ciphertext".to_vec() },
            Frame::Recv { from: k, flow: u32::MAX, packet: Vec::new() },
        ] {
            assert_eq!(Frame::read_from(&mut &f.encode()[..]).unwrap(), f);
        }
        let mut huge = vec![SEND];
        huge.extend_from_slice(&((36 + MAX_PACKET + 1) as u32).to_be_bytes());
        assert!(Frame::read_from(&mut &huge[..]).is_err());
        assert!(Frame::read_from(&mut &[99u8, 0, 0, 0, 0][..]).is_err());
        assert!(Frame::read_from(&mut &[HELLO, 0, 0, 0, 3, 1, 2, 3][..]).is_err());
    }
}

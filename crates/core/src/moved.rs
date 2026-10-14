//! A computer moving to another identity (docs/DESIGN.md section 3, "One
//! identity with windowcast"): a computer whose agent and windowcast host
//! each had a key and pairings keeps windowcast's, and tells each handheld
//! that still knows it by the old one, the next time it connects there.
//! Both keys sign the move, so only the holder of the key the handheld
//! pinned can name the new one.

use serde::{Deserialize, Serialize};

use crate::hex;
use crate::keys::{verify, DeviceKey, PeerId};

const LABEL: &[u8] = b"droidtop-agent moved v1\0";

/// The new identity, signed by the old key and the new one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The new PeerId, in hex.
    pub id: String,
    pub by_old: String,
    pub by_new: String,
}

fn statement(old: &PeerId, new: &PeerId) -> Vec<u8> {
    let mut s = LABEL.to_vec();
    s.extend_from_slice(&old.0);
    s.extend_from_slice(&new.0);
    s
}

impl Moved {
    pub fn new(old: &DeviceKey, new: &DeviceKey) -> Moved {
        let s = statement(&old.peer_id(), &new.peer_id());
        Moved { id: new.peer_id().to_hex(), by_old: hex::encode(&old.sign(&s)), by_new: hex::encode(&new.sign(&s)) }
    }

    /// The new identity, when both signatures hold for a move from [`old`].
    pub fn check(&self, old: &PeerId) -> Option<PeerId> {
        let new = PeerId::from_hex(&self.id).ok()?;
        let s = statement(old, &new);
        let ok = verify(old, &s, &hex::decode(&self.by_old)?) && verify(&new, &s, &hex::decode(&self.by_new)?);
        ok.then_some(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_needs_both_keys() {
        let (old, new, other) = (DeviceKey::generate(), DeviceKey::generate(), DeviceKey::generate());
        let moved = Moved::new(&old, &new);
        assert_eq!(moved.check(&old.peer_id()), Some(new.peer_id()));
        // Not a move from some other computer.
        assert_eq!(moved.check(&other.peer_id()), None);
        // Someone without the old key cannot name a new one.
        let forged = Moved::new(&other, &new);
        assert_eq!(forged.check(&old.peer_id()), None);
        let swapped = Moved { id: other.peer_id().to_hex(), ..moved };
        assert_eq!(swapped.check(&old.peer_id()), None);
    }
}

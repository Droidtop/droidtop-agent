//! A device's one key (docs/DESIGN.md section 3): windowcast's Ed25519
//! identity, whose public key is the device's [`PeerId`], and the same key
//! converted to X25519 for the Noise channel and WireGuard. The conversion is
//! the standard one (libsodium's `crypto_sign_ed25519_*_to_curve25519`): the
//! X25519 secret is the Ed25519 secret scalar, the X25519 public key is the
//! Ed25519 point in Montgomery form.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

pub use windowcast_identity::PeerId;

use crate::{Error, Result};

pub struct DeviceKey {
    signing: SigningKey,
}

impl DeviceKey {
    pub fn generate() -> Self {
        DeviceKey { signing: SigningKey::generate(&mut OsRng) }
    }

    /// The key from its 32-byte seed, the form windowcast's identity file
    /// holds and droidtop keeps sealed.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let seed: [u8; 32] = seed.try_into().map_err(|_| Error::BadKey)?;
        Ok(DeviceKey { signing: SigningKey::from_bytes(&seed) })
    }

    /// The key a windowcast [`windowcast_identity::Identity`] holds, so a
    /// computer that runs both keeps one identity.
    pub fn from_identity(identity: &windowcast_identity::Identity) -> Self {
        let pair = identity.to_keypair_bytes();
        DeviceKey { signing: SigningKey::from_keypair_bytes(&pair).expect("an Identity always holds a valid key pair") }
    }

    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// The X25519 secret: Noise and WireGuard clamp it the way Ed25519 does.
    pub fn x25519_secret(&self) -> [u8; 32] {
        self.signing.to_scalar_bytes()
    }

    pub fn x25519_public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_montgomery().to_bytes()
    }

    /// A shared secret with [`peer`] (X25519 between the two identities),
    /// for messages sealed at rest (the cloud share, [`crate::mailbox`]).
    pub fn shared_secret(&self, peer: &PeerId) -> Result<[u8; 32]> {
        Ok(x25519_dalek::x25519(self.x25519_secret(), x25519_public_of(peer)?))
    }
}

/// A peer's X25519 public key, derived from its PeerId.
pub fn x25519_public_of(peer: &PeerId) -> Result<[u8; 32]> {
    let key = VerifyingKey::from_bytes(&peer.0).map_err(|_| Error::BadKey)?;
    Ok(key.to_montgomery().to_bytes())
}

/// Whether [`signature`] is [`peer`]'s over [`message`].
pub fn verify(peer: &PeerId, message: &[u8], signature: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&peer.0) else { return false };
    let Ok(signature) = Signature::from_slice(signature) else { return false };
    key.verify(message, &signature).is_ok()
}

/// A short form of a PeerId for names and logs: its first 8 bytes in hex.
pub fn short(peer: &PeerId) -> String {
    crate::hex::encode(&peer.0[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_reach_the_same_x25519_secret() {
        let a = DeviceKey::generate();
        let b = DeviceKey::generate();
        assert_eq!(a.shared_secret(&b.peer_id()).unwrap(), b.shared_secret(&a.peer_id()).unwrap());
        assert_eq!(x25519_public_of(&a.peer_id()).unwrap(), a.x25519_public());
    }

    #[test]
    fn x25519_public_matches_the_secret() {
        let a = DeviceKey::generate();
        let base = x25519_dalek::X25519_BASEPOINT_BYTES;
        assert_eq!(x25519_dalek::x25519(a.x25519_secret(), base), a.x25519_public());
    }

    #[test]
    fn seed_round_trips_and_signs() {
        let a = DeviceKey::generate();
        let b = DeviceKey::from_seed(&a.seed()).unwrap();
        assert_eq!(a.peer_id(), b.peer_id());
        let sig = b.sign(b"hello");
        assert!(verify(&a.peer_id(), b"hello", &sig));
        assert!(!verify(&a.peer_id(), b"other", &sig));
    }
}

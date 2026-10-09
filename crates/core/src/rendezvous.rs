//! Finding a paired device away from the LAN (docs/DESIGN.md section 10,
//! "Rendezvous"), the way Syncthing does it: STUN for the address a NAT
//! gives a UDP socket, Syncthing's global discovery protocol (v3) for small
//! announcements of that address, and UDP hole punching between the two
//! devices. Only addresses travel through these services; every byte of a
//! sync goes through the direct WireGuard tunnel. Syncthing's relays are
//! never used.
//!
//! The mechanism is the `windowcast-rendezvous` crate, which windowcast
//! uses for its own sessions away from home; this module gives it
//! droidtop-agent's address scheme and discovery identity.
//!
//! - **Discovery identity.** Global discovery names a device by the SHA-256
//!   of the TLS client certificate it announces with (Syncthing's device ID).
//!   Each device's certificate is made from a key derived from its own seed
//!   and [`LABEL`], so the same seed always gives the same certificate and
//!   ID and nothing more is kept. The two devices tell each other their IDs
//!   in `hello`.

use std::net::SocketAddr;

pub use windowcast_rendezvous::*;

use crate::keys::DeviceKey;

/// The scheme of a WireGuard address in an announcement.
pub const SCHEME: &str = "wg://";

/// droidtop-agent's certificate label: windowcast on the same identity
/// uses its own, so the two are different devices to discovery.
pub const LABEL: &[u8] = b"droidtop-agent discovery certificate v1";

/// This device's discovery certificate.
pub fn certificate(key: &DeviceKey) -> Result<DiscoveryCert, String> {
    DiscoveryCert::derive(LABEL, &key.seed())
}

/// The WireGuard endpoints among announced addresses.
pub fn wg_endpoints(addresses: &[String]) -> Vec<SocketAddr> {
    endpoints(addresses, SCHEME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_ids_are_the_ones_devices_already_announce() {
        // The ID droidtop-agent's own implementation (77fb28e) gave this
        // seed: moving to the shared crate changes no device's ID.
        let key = DeviceKey::from_seed(&[7u8; 32]).unwrap();
        assert_eq!(certificate(&key).unwrap().device_id(), "JUUMALY-C6VHHQC-ZWHS7I5-HBGFFHR-VCT5MHY-HNETOVA-XBZOFZP-JL5RMQ2");
        assert_ne!(certificate(&DeviceKey::generate()).unwrap().device_id(), certificate(&key).unwrap().device_id());
    }

    #[test]
    fn wireguard_addresses_are_picked_out() {
        assert_eq!(
            wg_endpoints(&["wg://203.0.113.7:47611".into(), "windowcast://203.0.113.7:47101".into()]),
            vec![SocketAddr::from(([203, 0, 113, 7], 47611))]
        );
    }
}

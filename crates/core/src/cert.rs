//! A device's certificate (docs/DESIGN.md section 4, Droidtop/tracker#556):
//! a self-signed X.509 certificate over the device's one Ed25519 key, made
//! by the device itself and issued by no one. Pairing pins it, and every
//! session is mutual TLS 1.3 in which each side presents its own and checks
//! the other's against what it pinned.
//!
//! The pin is the certificate's public key, not its bytes: the SHA-256 of
//! its SubjectPublicKeyInfo ([`fingerprint`]), the usual public-key pin. A
//! device can reissue its certificate (another validity, another library
//! version encoding it differently) without being paired again, and a
//! device paired by its [`PeerId`] before certificates existed is the same
//! pin, since an Ed25519 SubjectPublicKeyInfo is a fixed prefix and the
//! key. A new key is a new identity: [`crate::moved`] hands it over.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::keys::{short, DeviceKey, PeerId};
use crate::{hex, Error, Result};

/// An Ed25519 SubjectPublicKeyInfo before its 32-byte key (RFC 8410).
const ED25519_SPKI: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// An Ed25519 private key in PKCS #8 (v1) before its 32-byte seed (RFC 8410).
const ED25519_PKCS8: [u8; 16] = [0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];

/// The pin of [`peer`]'s certificate: SHA-256 of its SubjectPublicKeyInfo.
pub fn fingerprint(peer: &PeerId) -> [u8; 32] {
    let mut spki = ED25519_SPKI.to_vec();
    spki.extend_from_slice(&peer.0);
    Sha256::digest(&spki).into()
}

/// [`fingerprint`] in hex, as pairing carries it and screens show it.
pub fn fingerprint_hex(peer: &PeerId) -> String {
    hex::encode(&fingerprint(peer))
}

/// [`key`]'s certificate: self-signed, its subject the device's short id,
/// valid from 2020 to 9999 (the pin, not a date, is what is trusted).
pub fn certificate(key: &DeviceKey) -> Result<CertificateDer<'static>> {
    let pair = rcgen::KeyPair::try_from(pkcs8(key).secret_pkcs8_der()).map_err(|e| Error::Tls(e.to_string()))?;
    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, format!("droidtop {}", short(&key.peer_id())));
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(9999, 12, 31);
    params.serial_number = Some(rcgen::SerialNumber::from_slice(&key.peer_id().0[..16]));
    let cert = params.self_signed(&pair).map_err(|e| Error::Tls(e.to_string()))?;
    Ok(cert.der().clone())
}

fn pkcs8(key: &DeviceKey) -> PrivatePkcs8KeyDer<'static> {
    let mut der = ED25519_PKCS8.to_vec();
    der.extend_from_slice(&key.seed());
    PrivatePkcs8KeyDer::from(der)
}

/// The device a certificate is for: the Ed25519 key it carries. Any other
/// kind of key is refused.
pub fn peer_of(cert: &CertificateDer<'_>) -> Result<PeerId> {
    let parsed = webpki::EndEntityCert::try_from(cert).map_err(|e| Error::Tls(format!("an unreadable certificate ({e:?})")))?;
    let spki = parsed.subject_public_key_info();
    let bytes = spki.as_ref();
    match bytes.strip_prefix(&ED25519_SPKI[..]) {
        Some(key) if key.len() == 32 => Ok(PeerId(key.try_into().expect("32 bytes"))),
        _ => Err(Error::Tls("a certificate without an Ed25519 device key".into())),
    }
}

/// The name a client asks for when it connects to [`peer`]: a computer
/// moving between two identities answers with the certificate it names.
pub fn server_name(peer: &PeerId) -> ServerName<'static> {
    ServerName::try_from(format!("{}.droidtop.invalid", short(peer))).expect("a valid DNS name")
}

/// Ed25519 and nothing else, both for handshake signatures and certificates.
static ALGORITHMS: WebPkiSupportedAlgorithms =
    WebPkiSupportedAlgorithms { all: &[webpki::ring::ED25519], mapping: &[(SignatureScheme::ED25519, &[webpki::ring::ED25519])] };

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certified(key: &DeviceKey) -> Result<Arc<CertifiedKey>> {
    let signer = provider().key_provider.load_private_key(PrivateKeyDer::Pkcs8(pkcs8(key)))?;
    Ok(Arc::new(CertifiedKey::new(vec![certificate(key)?], signer)))
}

fn tls13(
    dss: &DigitallySignedStruct,
    message: &[u8],
    cert: &CertificateDer<'_>,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    verify_tls13_signature(message, cert, dss, &ALGORITHMS)
}

/// The client's check of the server: its certificate carries the key pinned for it.
#[derive(Debug)]
struct PinnedServer(PeerId);

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        match peer_of(end_entity) {
            Ok(peer) if peer == self.0 => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        tls13(dss, message, cert)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ALGORITHMS.supported_schemes()
    }
}

/// The server's check of the client: a certificate carrying a device key,
/// whose handshake signature holds. Whether that device is paired is asked
/// once the handshake is done ([`crate::channel::Channel::accept_any`]).
#[derive(Debug)]
struct AnyDevice;

impl ClientCertVerifier for AnyDevice {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        peer_of(end_entity)
            .map(|_| ClientCertVerified::assertion())
            .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        tls13(dss, message, cert)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ALGORITHMS.supported_schemes()
    }
}

/// The server's certificates: the one the client named ([`server_name`]),
/// else the first (the current identity).
#[derive(Debug)]
struct ByName(Vec<(String, Arc<CertifiedKey>)>);

impl ResolvesServerCert for ByName {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let named = hello.server_name().and_then(|n| self.0.iter().find(|(name, _)| name == n));
        named.or(self.0.first()).map(|(_, k)| k.clone())
    }
}

/// The handshake for the side that connects to [`server`].
pub fn client_config(key: &DeviceKey, server: &PeerId) -> Result<rustls::ClientConfig> {
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer(*server)))
        .with_client_auth_cert(vec![certificate(key)?], PrivateKeyDer::Pkcs8(pkcs8(key)))?;
    config.resumption = rustls::client::Resumption::disabled();
    Ok(config)
}

/// The handshake for the side that accepts, answering to each of [`keys`]
/// (the first is its current identity).
pub fn server_config(keys: &[&DeviceKey]) -> Result<rustls::ServerConfig> {
    let certs = keys.iter().map(|k| Ok((server_name(&k.peer_id()).to_str().into_owned(), certified(k)?))).collect::<Result<Vec<_>>>()?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AnyDevice))
        .with_cert_resolver(Arc::new(ByName(certs)));
    config.send_tls13_tickets = 0;
    Ok(config)
}

/// Which of [`keys`] a client asked for by [`server_name`] (0 when it named none).
pub fn named_index(keys: &[&DeviceKey], name: Option<&str>) -> usize {
    name.and_then(|n| keys.iter().position(|k| server_name(&k.peer_id()).to_str() == n)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_carries_its_device_key_and_pins_by_it() {
        let key = DeviceKey::generate();
        let cert = certificate(&key).unwrap();
        assert_eq!(peer_of(&cert).unwrap(), key.peer_id());
        // The pin is the key's: a reissued certificate keeps it.
        let parsed = webpki::EndEntityCert::try_from(&cert).unwrap();
        let spki: [u8; 32] = Sha256::digest(parsed.subject_public_key_info().as_ref()).into();
        assert_eq!(spki, fingerprint(&key.peer_id()));
        assert_eq!(fingerprint_hex(&key.peer_id()).len(), 64);
        assert_ne!(fingerprint(&key.peer_id()), fingerprint(&DeviceKey::generate().peer_id()));
    }

    #[test]
    fn a_name_picks_the_identity_a_client_asked_for() {
        let (new, old) = (DeviceKey::generate(), DeviceKey::generate());
        let keys = [&new, &old];
        let asked = server_name(&old.peer_id()).to_str().into_owned();
        assert_eq!(named_index(&keys, Some(&asked)), 1);
        assert_eq!(named_index(&keys, None), 0);
        assert_eq!(named_index(&keys, Some("other.droidtop.invalid")), 0);
    }
}

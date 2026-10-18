//! Test PKI generated with rcgen. Shared by the unit tests in `src/tls` and
//! the integration tests, so no PEM file is ever committed.
#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair, KeyUsagePurpose,
    RevokedCertParams, SanType, SerialNumber,
};
use rustls::pki_types::CertificateRevocationListDer;

pub struct Ca {
    pub cert: rcgen::Certificate,
    key: KeyPair,
}

pub struct Leaf {
    pub cert: rcgen::Certificate,
    pub key: KeyPair,
}

/// A URI subjectAltName.
pub fn uri_san(uri: &str) -> SanType {
    SanType::URI(uri.try_into().unwrap())
}

impl Ca {
    pub fn new(name: &str) -> Self {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        Self { cert, key }
    }

    /// Server certificate for `localhost` and `127.0.0.1`, identity
    /// `monitorflow://server/central`.
    pub fn server(&self) -> Leaf {
        let sans = vec![
            SanType::DnsName("localhost".try_into().unwrap()),
            SanType::IpAddress(Ipv4Addr::LOCALHOST.into()),
            uri_san("monitorflow://server/central"),
        ];
        let key = KeyPair::generate().unwrap();
        self.sign("server", sans, 1, ExtendedKeyUsagePurpose::ServerAuth, key)
    }

    /// Agent client certificate, identity `monitorflow://agent/<id>`.
    pub fn client(&self, id: &str, serial: u64) -> Leaf {
        self.client_with_key(id, serial, KeyPair::generate().unwrap())
    }

    pub fn client_with_key(&self, id: &str, serial: u64, key: KeyPair) -> Leaf {
        let sans = vec![uri_san(&format!("monitorflow://agent/{id}"))];
        self.sign(id, sans, serial, ExtendedKeyUsagePurpose::ClientAuth, key)
    }

    /// Relay client certificate, identity `monitorflow://relay/<id>`.
    pub fn relay(&self, id: &str, serial: u64) -> Leaf {
        let sans = vec![uri_san(&format!("monitorflow://relay/{id}"))];
        let key = KeyPair::generate().unwrap();
        self.sign(id, sans, serial, ExtendedKeyUsagePurpose::ClientAuth, key)
    }

    /// Client certificate with arbitrary subjectAltNames.
    pub fn leaf(&self, cn: &str, sans: Vec<SanType>, serial: u64) -> Leaf {
        let key = KeyPair::generate().unwrap();
        self.sign(cn, sans, serial, ExtendedKeyUsagePurpose::ClientAuth, key)
    }

    fn sign(
        &self,
        cn: &str,
        sans: Vec<SanType>,
        serial: u64,
        eku: ExtendedKeyUsagePurpose,
        key: KeyPair,
    ) -> Leaf {
        let mut params = CertificateParams::default();
        // rcgen adds a default CN; start from an empty subject.
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, cn);
        params.subject_alt_names = sans;
        params.serial_number = Some(SerialNumber::from(serial));
        params.extended_key_usages = vec![eku];
        let cert = params.signed_by(&key, &self.cert, &self.key).unwrap();
        Leaf { cert, key }
    }

    /// CRL revoking the given serial numbers.
    pub fn crl(&self, serials: &[u64]) -> CertificateRevocationListDer<'static> {
        let now = rcgen::date_time_ymd(2026, 1, 1);
        let params = CertificateRevocationListParams {
            this_update: now,
            next_update: rcgen::date_time_ymd(2100, 1, 1),
            crl_number: SerialNumber::from(1),
            issuing_distribution_point: None,
            revoked_certs: serials
                .iter()
                .map(|&s| RevokedCertParams {
                    serial_number: SerialNumber::from(s),
                    revocation_time: now,
                    reason_code: None,
                    invalidity_date: None,
                })
                .collect(),
            key_identifier_method: KeyIdMethod::Sha256,
        };
        params
            .signed_by(&self.cert, &self.key)
            .unwrap()
            .der()
            .clone()
    }
}

/// Writes `ca.pem`, `<name>.pem` and `<name>-key.pem` to `dir` and returns
/// their paths in that order.
pub fn write(dir: &Path, name: &str, ca: &Ca, leaf: &Leaf) -> (PathBuf, PathBuf, PathBuf) {
    let ca_path = dir.join("ca.pem");
    let cert = dir.join(format!("{name}.pem"));
    let key = dir.join(format!("{name}-key.pem"));
    std::fs::write(&ca_path, ca.cert.pem()).unwrap();
    std::fs::write(&cert, leaf.cert.pem()).unwrap();
    std::fs::write(&key, leaf.key.serialize_pem()).unwrap();
    (ca_path, cert, key)
}

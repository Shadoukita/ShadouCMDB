//! TLS for the connections to identity providers (OIDC over HTTPS, LDAPS and
//! StartTLS): rustls with ring, certificate chain and host name always
//! verified. Trusted: the Mozilla root set, the operating system's store (where
//! a company's private CA usually lives) and the provider's own CA PEM.

use std::sync::{Arc, LazyLock};

use rustls::client::ClientConfig;
use rustls::{RootCertStore, crypto::ring};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

/// The operating system's trust store, read once per process. Unreadable
/// entries are skipped (a store with one broken file must not stop sign-in).
static NATIVE_ROOTS: LazyLock<Vec<CertificateDer<'static>>> = LazyLock::new(|| {
    let found = rustls_native_certs::load_native_certs();
    if !found.errors.is_empty() {
        tracing::debug!(errors = found.errors.len(), "some operating-system CA certificates could not be read");
    }
    found.certs
});

/// The operating system's trust store (read once per process), for clients
/// that build their own root store (workflow e-mail).
pub fn native_roots() -> &'static [CertificateDer<'static>] {
    &NATIVE_ROOTS
}

/// Parses a PEM bundle; fails when it holds no certificate or a broken one.
pub fn parse_ca_pem(pem: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("not a valid PEM certificate bundle ({e})"))?;
    if certs.is_empty() {
        return Err("contains no certificate (expected -----BEGIN CERTIFICATE-----)".into());
    }
    let mut store = RootCertStore::empty();
    let (_, rejected) = store.add_parsable_certificates(certs.iter().cloned());
    if rejected > 0 {
        return Err(format!("{rejected} of the certificates could not be parsed"));
    }
    Ok(certs)
}

/// A client configuration that trusts the public roots, the OS store and `ca_pem`.
pub fn client_config(ca_pem: Option<&str>) -> Result<ClientConfig, String> {
    let mut roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    roots.add_parsable_certificates(NATIVE_ROOTS.iter().cloned());
    if let Some(pem) = ca_pem {
        for cert in parse_ca_pem(pem)? {
            roots.add(cert).map_err(|e| format!("CA certificate refused: {e}"))?;
        }
    }
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())
        .map(|b| b.with_root_certificates(roots).with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pem_without_certificates_is_refused() {
        assert!(parse_ca_pem("").is_err());
        assert!(parse_ca_pem("hello").is_err());
        let broken = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        assert!(parse_ca_pem(broken).is_err());
        assert!(client_config(None).is_ok());
    }
}

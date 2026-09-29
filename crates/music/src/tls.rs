//! TLS verification opt-outs for servers the user runs themselves.
//!
//! A self-hosted server often answers HTTPS with a certificate no public CA signed. A user can
//! accept one anyway, and the authority it serves lands in the registry here. `builder` and
//! `client` hand out reqwest clients whose certificate verifier consults the registry first,
//! so every request path, API, stream, artwork and scrobble alike, honours the opt-in without
//! each call site having to pick a client. `trusted` answers for callers that never carry a
//! request at all, and `authority` maps a URL to the name the registry stores.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

/// The authorities (host or host:port) whose certificates are accepted without verification.
fn trusted_set() -> &'static Mutex<HashSet<String>> {
    static TRUSTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    TRUSTED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Marks an authority's certificate as accepted even when the system roots reject it.
pub fn trust(authority: &str) {
    if let Ok(mut trusted) = trusted_set().lock() {
        trusted.insert(authority.to_owned());
    }
}

/// Drops an authority from the trusted set, for when the account it served is forgotten.
pub fn distrust(authority: &str) {
    if let Ok(mut trusted) = trusted_set().lock() {
        trusted.remove(authority);
    }
}

/// Whether `authority` has opted out of certificate verification.
pub fn trusted(authority: &str) -> bool {
    trusted_set()
        .lock()
        .is_ok_and(|trusted| trusted.contains(authority))
}

/// Whether `name`, the SNI a certificate is presented for, names an accepted authority. The
/// registry can carry `host:port` while SNI is always bare, so a `host:` prefix counts too.
fn trusted_name(name: &str) -> bool {
    let Ok(trusted) = trusted_set().lock() else {
        return false;
    };
    if trusted.contains(name) {
        return true;
    }
    let prefix = format!("{name}:");
    trusted.iter().any(|entry| entry.starts_with(&prefix))
}

/// The authority part of a URL: `https://nas.example:4533/rest` gives `nas.example:4533`.
pub fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then_some(authority)
}

/// Wraps the platform verifier so the trust registry is asked before any chain building runs.
/// An authority the user accepted passes without the inner verifier running, which also keeps
/// its error logging out of the log for a decision the user already made. Every other
/// authority verifies exactly as it always did.
#[derive(Debug)]
struct RegistryVerifier {
    inner: rustls_platform_verifier::Verifier,
}

impl rustls::client::danger::ServerCertVerifier for RegistryVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if trusted_name(server_name.to_str().as_ref()) {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// The rustls config carrying the registry-aware verifier, or a plain error when the platform
/// verifier cannot be built at all, in which case callers fall back to reqwest defaults.
fn config() -> anyhow::Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls_platform_verifier::Verifier::new(provider.clone())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(RegistryVerifier { inner: verifier }))
        .with_no_client_auth();
    Ok(config)
}

/// A `reqwest` client builder whose certificate verifier honours the trust registry. Callers
/// that customise the client, such as a user agent, start here instead of `Client::builder`.
pub fn builder() -> reqwest::ClientBuilder {
    match config() {
        Ok(config) => reqwest::Client::builder().tls_backend_preconfigured(config),
        Err(error) => {
            log::warn!("tls: the registry-aware verifier cannot be built: {error:#}");
            reqwest::Client::builder()
        }
    }
}

/// The shared client for callers that need nothing custom. Verification is per the registry:
/// plain platform roots for everyone, a pass for the authorities the user accepted.
pub fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| builder().build().unwrap_or_default())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_host_answers_trusted() {
        trust("nas.example");
        assert!(trusted_name("nas.example"));
        assert!(!trusted_name("other.example"));
        distrust("nas.example");
    }

    #[test]
    fn a_registered_host_and_port_match_the_bare_sni_name() {
        trust("nas.example:4533");
        assert!(trusted_name("nas.example"));
        assert!(!trusted_name("nas.example.evil"));
        distrust("nas.example:4533");
    }

    #[test]
    fn a_name_that_only_shares_a_prefix_is_not_trusted() {
        trust("nas");
        assert!(!trusted_name("nasal.example"));
        assert!(!trusted_name("nasa"));
        distrust("nas");
    }

    #[test]
    fn authority_splits_the_scheme_and_path() {
        assert_eq!(
            authority("https://nas.example:4533/rest"),
            Some("nas.example:4533")
        );
        assert_eq!(authority("https://nas.example"), Some("nas.example"));
        assert_eq!(authority("nas.example/rest"), Some("nas.example"));
        assert_eq!(authority("https://"), None);
    }
}

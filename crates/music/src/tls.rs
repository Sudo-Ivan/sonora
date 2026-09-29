//! TLS verification opt-outs for servers the user runs themselves.
//!
//! A self-hosted server often answers HTTPS with a certificate no public CA signed. A user can
//! accept one anyway, and the authority it serves lands in the registry here: [`trusted`]
//! answers for callers that never hear the preference directly, such as the artwork fetcher
//! that only sees a URL, and [`insecure`] hands out the unverifying client for callers that
//! carry the preference themselves.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

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

/// The authority part of a URL: `https://nas.example:4533/rest` gives `nas.example:4533`.
pub fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then_some(authority)
}

/// A client that accepts any certificate. Only ever reached for a host the user named, never
/// as a default, so the one pool it holds stays small.
pub fn insecure() -> reqwest::Client {
    static INSECURE: OnceLock<reqwest::Client> = OnceLock::new();
    INSECURE
        .get_or_init(|| {
            reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap_or_default()
        })
        .clone()
}

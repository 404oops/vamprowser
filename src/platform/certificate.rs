//! Browser-owned, certificate-specific exceptions. Never changes system trust.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use objc2_security::SecTrust;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x509_parser::{extensions::GeneralName, parse_x509_certificate};

#[derive(Clone, Debug)]
pub struct Presented {
    pub host: String,
    pub port: isize,
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    pub valid_from: String,
    pub valid_until: String,
    pub names: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Exception {
    host: String,
    port: isize,
    fingerprint: String,
}

static EXCEPTIONS: OnceLock<Mutex<Vec<Exception>>> = OnceLock::new();
static SESSION: OnceLock<Mutex<HashMap<u64, Vec<Exception>>>> = OnceLock::new();

fn exceptions() -> &'static Mutex<Vec<Exception>> {
    EXCEPTIONS.get_or_init(|| {
        Mutex::new(
            crate::state::load_json(crate::state::data_path("certificate-exceptions.json"))
                .unwrap_or_default(),
        )
    })
}

fn session() -> &'static Mutex<HashMap<u64, Vec<Exception>>> {
    SESSION.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn presented(trust: &SecTrust, host: &str, port: isize) -> Option<Presented> {
    // Security still exposes the leaf through this API on macOS; the newer
    // chain API requires untyped Core Foundation array elements.
    #[allow(deprecated)]
    // SAFETY: the first certificate and its copied DER belong to this live trust.
    let cert = unsafe { trust.certificate_at_index(0)? };
    let der = unsafe { cert.data() };
    let bytes = unsafe { std::slice::from_raw_parts(der.byte_ptr(), der.length() as usize) };
    let (_, parsed) = parse_x509_certificate(bytes).ok()?;
    let fingerprint = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let names = parsed
        .subject_alternative_name()
        .ok()
        .flatten()
        .map_or_else(Vec::new, |ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(name) => Some((*name).to_owned()),
                    GeneralName::IPAddress(bytes) if bytes.len() == 4 => Some(format!(
                        "{}.{}.{}.{}",
                        bytes[0], bytes[1], bytes[2], bytes[3]
                    )),
                    GeneralName::IPAddress(bytes) if bytes.len() == 16 => {
                        <[u8; 16]>::try_from(*bytes)
                            .ok()
                            .map(|addr| std::net::Ipv6Addr::from(addr).to_string())
                    }
                    _ => None,
                })
                .collect()
        });
    Some(Presented {
        host: host.to_ascii_lowercase(),
        port,
        fingerprint,
        subject: parsed.subject().to_string(),
        issuer: parsed.issuer().to_string(),
        valid_from: parsed.validity().not_before.to_datetime().to_string(),
        valid_until: parsed.validity().not_after.to_datetime().to_string(),
        names,
    })
}

pub fn is_trusted(cert: &Presented, scope: u64) -> bool {
    let matches = |entry: &Exception| {
        entry.host == cert.host && entry.port == cert.port && entry.fingerprint == cert.fingerprint
    };
    if scope == 0 {
        exceptions().lock().unwrap().iter().any(matches)
    } else {
        session()
            .lock()
            .unwrap()
            .get(&scope)
            .is_some_and(|entries| entries.iter().any(matches))
    }
}

pub fn trust(cert: &Presented, scope: u64) -> std::io::Result<()> {
    let entry = Exception {
        host: cert.host.clone(),
        port: cert.port,
        fingerprint: cert.fingerprint.clone(),
    };
    if scope != 0 {
        session()
            .lock()
            .unwrap()
            .entry(scope)
            .or_default()
            .push(entry);
        return Ok(());
    }
    {
        let mut entries = exceptions().lock().unwrap();
        if !entries.iter().any(|saved| {
            saved.host == entry.host
                && saved.port == entry.port
                && saved.fingerprint == entry.fingerprint
        }) {
            entries.push(entry);
            let path = crate::state::data_path("certificate-exceptions.json")
                .ok_or_else(|| std::io::Error::other("HOME is unset"))?;
            if let Err(error) =
                crate::state::write_atomic(&path, &serde_json::to_vec_pretty(&*entries)?)
            {
                entries.pop();
                return Err(error);
            }
        }
        Ok(())
    }
}

pub fn forget_private(scope: u64) {
    session().lock().unwrap().remove(&scope);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_bound_to_host_port_and_exact_certificate() {
        let cert = Presented {
            host: "device.local".into(),
            port: 8006,
            fingerprint: "AA:BB".into(),
            subject: String::new(),
            issuer: String::new(),
            valid_from: String::new(),
            valid_until: String::new(),
            names: Vec::new(),
        };
        let scope = 987654321;
        trust(&cert, scope).unwrap();
        assert!(is_trusted(&cert, scope));
        let other_thread = cert.clone();
        assert!(
            std::thread::spawn(move || is_trusted(&other_thread, scope))
                .join()
                .unwrap()
        );
        assert!(!is_trusted(
            &Presented {
                host: "other.local".into(),
                ..cert.clone()
            },
            scope
        ));
        assert!(!is_trusted(
            &Presented {
                port: 443,
                ..cert.clone()
            },
            scope
        ));
        assert!(!is_trusted(
            &Presented {
                fingerprint: "CC:DD".into(),
                ..cert.clone()
            },
            scope
        ));
        assert!(!is_trusted(&cert, scope + 1));
        forget_private(scope);
        assert!(!is_trusted(&cert, scope));
    }
}

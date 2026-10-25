//! Bounded PEM/DER file loading for the reference server and tools.
//!
//! Files are read with a hard size cap. Parse errors name the file path but never include file
//! contents. A file without a `-----BEGIN` marker is treated as raw DER (PKCS#8 for keys).
use crate::OnlineError;
use base64::{Engine, engine::general_purpose::STANDARD};
use engine_network::{
    CertificateDer, PrivateKeyDer,
    rustls::pki_types::{PrivatePkcs8KeyDer, pem::PemObject},
};
use std::{fs::File, io::Read, path::Path};
use zeroize::Zeroizing;

/// Largest file this module reads.
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// PEM-armors `der` with the given label, 64 columns per line.
pub fn encode(label: &str, der: &[u8]) -> String {
    let body = STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// Reads at most [`MAX_FILE_BYTES`]; larger files are refused.
pub fn read_file(path: &Path) -> Result<Vec<u8>, OnlineError> {
    let describe = |what: &str| OnlineError::Config(format!("{what}: {}", path.display()));
    let file = File::open(path).map_err(|_| describe("cannot open"))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| describe("cannot read"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(describe("file too large"));
    }
    Ok(bytes)
}

/// Like [`read_file`] for secrets: the buffer is wiped when dropped.
fn read_secret(path: &Path) -> Result<Zeroizing<Vec<u8>>, OnlineError> {
    read_file(path).map(Zeroizing::new)
}

/// Refuses (on Unix) a secret file that group or others can access, like `ssh` does for keys.
/// Other platforms rely on the directory ACL the operator configured.
pub fn ensure_owner_only(path: &Path) -> Result<(), OnlineError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(path)
            .map_err(|_| OnlineError::Config(format!("cannot stat {}", path.display())))?
            .mode();
        if mode & 0o077 != 0 {
            return Err(OnlineError::Config(format!(
                "{} must not be accessible by group or others (chmod 600)",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn is_pem(bytes: &[u8]) -> bool {
    bytes.windows(10).any(|w| w == b"-----BEGIN")
}

/// All certificates in a PEM file (leaf first), or one DER certificate.
pub fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, OnlineError> {
    let bytes = read_file(path)?;
    let bad = || OnlineError::Config(format!("no valid certificate in {}", path.display()));
    let certificates: Vec<CertificateDer<'static>> = if is_pem(&bytes) {
        CertificateDer::pem_slice_iter(&bytes)
            .collect::<Result<_, _>>()
            .map_err(|_| bad())?
    } else {
        vec![CertificateDer::from(bytes)]
    };
    if certificates.is_empty() || certificates.len() > 16 {
        return Err(bad());
    }
    Ok(certificates)
}

/// A private key from PEM (`PRIVATE KEY`, `RSA PRIVATE KEY` or `EC PRIVATE KEY`) or PKCS#8 DER.
pub fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, OnlineError> {
    let bytes = read_secret(path)?;
    let bad = || OnlineError::Config(format!("no valid private key in {}", path.display()));
    if is_pem(&bytes) {
        PrivateKeyDer::from_pem_slice(&bytes).map_err(|_| bad())
    } else {
        Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            bytes.to_vec(),
        )))
    }
}

/// PKCS#8 DER of an Ed25519 signing key from PEM (`PRIVATE KEY`) or raw DER.
pub fn load_pkcs8(path: &Path) -> Result<Zeroizing<Vec<u8>>, OnlineError> {
    let bytes = read_secret(path)?;
    if is_pem(&bytes) {
        PrivatePkcs8KeyDer::from_pem_slice(&bytes)
            .map(|key| Zeroizing::new(key.secret_pkcs8_der().to_vec()))
            .map_err(|_| OnlineError::Config(format!("no valid PKCS#8 key in {}", path.display())))
    } else {
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_round_trip_and_size_cap() {
        let dir = std::env::temp_dir().join(format!("engine-online-pem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let der = vec![7u8; 200];
        let path = dir.join("k.pem");
        std::fs::write(&path, encode("PRIVATE KEY", &der)).unwrap();
        assert_eq!(load_pkcs8(&path).unwrap().as_slice(), der.as_slice());
        let raw = dir.join("k.der");
        std::fs::write(&raw, &der).unwrap();
        assert_eq!(load_pkcs8(&raw).unwrap().as_slice(), der.as_slice());
        let big = dir.join("big");
        std::fs::write(&big, vec![0u8; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_file(&big).is_err());
        assert!(read_file(&dir.join("missing")).is_err());
        let garbage = dir.join("garbage.pem");
        std::fs::write(
            &garbage,
            "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert!(load_certificates(&garbage).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let secret = dir.join("secret");
            std::fs::write(&secret, b"x").unwrap();
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(ensure_owner_only(&secret).is_err());
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(ensure_owner_only(&secret).is_ok());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

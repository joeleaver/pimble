//! The plugin library (docs/SETTINGS_CONTRACT.md section 4): the index of
//! plugins pimble.app offers, which of them this device should install or
//! update, and the check every downloaded plugin passes before it is installed.
//! Pure: the app fetches, this decides and verifies, on both targets.

use std::collections::BTreeMap;

use pimble_plugin_abi::{Manifest, Permissions, ABI_VERSION};
use serde::{Deserialize, Serialize};

/// The version of the index's format this app reads.
pub const INDEX_FORMAT: u32 = 1;

/// The index file a release carries (`plugins.json`).
pub const INDEX_FILE: &str = "plugins.json";

/// Every plugin the library offers, as the latest release lists them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub v: u32,
    pub plugins: Vec<Entry>,
}

/// One plugin in the library, read from its own manifest by
/// `pimble-plugin-index` when the release was built.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub abi: u32,
    pub author: String,
    pub summary: String,
    pub asks: Permissions,
    /// The release's file names: the module, its detached signature, and an
    /// optional screenshot.
    pub wasm: String,
    pub sig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<String>,
    pub bytes: u64,
    /// The module's SHA-256, in lowercase hex.
    pub sha256: String,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        #[derive(Deserialize)]
        struct Format {
            v: u32,
        }
        let format: Format = serde_json::from_slice(bytes).map_err(|e| format!("the library's index does not read: {e}"))?;
        if format.v != INDEX_FORMAT {
            return Err("The library is in a newer format than this Pimble reads. Update Pimble to see it.".to_string());
        }
        serde_json::from_slice(bytes).map_err(|e| format!("the library's index does not read: {e}"))
    }

    /// Every file the index names, which is all the accounts service serves.
    pub fn names(&self, file: &str) -> bool {
        self.plugins.iter().any(|e| e.wasm == file || e.sig == file || e.shot.as_deref() == Some(file))
    }
}

/// What the library's row for a plugin offers on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// Not installed here.
    Install,
    /// Installed from the library at this version or a later one.
    Installed,
    /// Installed from the library at an earlier version.
    Update { from: String },
    /// Installed here from a file: the library leaves it alone.
    FromFile,
    /// Built for another plugin ABI than this Pimble runs.
    NeedsNewer,
}

/// What `entry` offers, given the version installed here (if any) and whether
/// that one came from the library.
pub fn offer(entry: &Entry, installed: Option<&str>, from_library: bool) -> Offer {
    if entry.abi != ABI_VERSION {
        // One installed and running stays; only the library's is out of reach.
        return match installed {
            Some(_) if from_library => Offer::Installed,
            Some(_) => Offer::FromFile,
            None => Offer::NeedsNewer,
        };
    }
    match installed {
        None => Offer::Install,
        Some(_) if !from_library => Offer::FromFile,
        Some(have) if newer(&entry.version, have) => Offer::Update { from: have.to_string() },
        Some(_) => Offer::Installed,
    }
}

/// Is `version` later than `than`? Anything that is not a version is never
/// later.
pub fn newer(version: &str, than: &str) -> bool {
    let parse = |v: &str| semver::Version::parse(v.trim_start_matches('v')).ok();
    match (parse(version), parse(than)) {
        (Some(v), Some(t)) => v > t,
        _ => false,
    }
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Does `signature` sign `bytes` with `public_key` (the raw 32 bytes of an
/// Ed25519 key in base64, as `tools/update-key.sh` writes it)? The one check
/// for everything Pimble installs: updates and library plugins.
pub fn verify_signature(public_key: &str, bytes: &[u8], signature: &[u8]) -> Result<(), String> {
    use base64::Engine as _;
    use ed25519_dalek::{Signature, VerifyingKey};
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(public_key.trim())
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or("this build has no signing key (tools/update-key.sh)")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|e| format!("the signing key: {e}"))?;
    let signature = Signature::from_slice(signature).map_err(|e| format!("the signature: {e}"))?;
    key.verify_strict(bytes, &signature).map_err(|_| "the signature does not verify".to_string())
}

/// The sentence on a row whose download did not pass [`verify_download`].
pub const NOT_VERIFIED: &str = "The download did not verify, so nothing was installed.";

/// Is `bytes`, downloaded for `entry`, what the library published: its length,
/// its SHA-256 and its signature under `public_key`?
pub fn verify_download(entry: &Entry, public_key: &str, bytes: &[u8], signature: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 != entry.bytes {
        return Err(format!("{} is {} bytes, not the {} the library lists", entry.wasm, bytes.len(), entry.bytes));
    }
    if sha256_hex(bytes) != entry.sha256.to_ascii_lowercase() {
        return Err(format!("{} does not have the SHA-256 the library lists", entry.wasm));
    }
    verify_signature(public_key, bytes, signature).map_err(|e| format!("{}: {e}", entry.wasm))
}

/// Does the plugin that loaded from a verified download say it is the one the
/// library listed?
pub fn matches(entry: &Entry, manifest: &Manifest) -> Result<(), String> {
    if manifest.id != entry.id || manifest.version != entry.version {
        return Err(format!(
            "{} says it is {} {}, not the library's {} {}",
            entry.wasm, manifest.id, manifest.version, entry.id, entry.version
        ));
    }
    Ok(())
}

/// The entry for a plugin built for a release, read from its own manifest.
pub fn entry_for(manifest: &Manifest, summary: &str, bytes: &[u8], shot: Option<String>) -> Entry {
    let stem = format!("{}-{}", manifest.id, manifest.version);
    Entry {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        abi: manifest.abi,
        author: "Pimble".to_string(),
        summary: summary.to_string(),
        asks: manifest.asks,
        wasm: format!("{stem}.wasm"),
        sig: format!("{stem}.wasm.sig"),
        shot,
        bytes: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    }
}

/// Which installed plugins came from the library, and at which version
/// (`<plugins dir>/library.json`, `localStorage` in the browser). A plugin not
/// named here was added from a file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub plugins: BTreeMap<String, String>,
}

impl Provenance {
    pub fn parse(bytes: &[u8]) -> Provenance {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    pub fn to_json(&self) -> Vec<u8> {
        let mut out = self.clone();
        out.v = 1;
        serde_json::to_vec_pretty(&out).unwrap_or_default()
    }

    pub fn from_library(&self, id: &str) -> bool {
        self.plugins.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_plugin_abi::StoreAsk;

    fn manifest(id: &str, version: &str) -> Manifest {
        Manifest {
            id: id.into(),
            name: "Kanban".into(),
            version: version.into(),
            abi: ABI_VERSION,
            rinch: "unknown".into(),
            provides: Vec::new(),
            asks: Permissions { stores: StoreAsk::Chosen, write: true, ..Default::default() },
        }
    }

    fn key_pair() -> (ed25519_dalek::SigningKey, String) {
        use base64::Engine as _;
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().as_bytes());
        (key, public)
    }

    #[test]
    fn an_index_reads_back_and_names_only_its_files() {
        let bytes = b"\0asm-kanban";
        let entry = entry_for(&manifest("kanban-viewer", "0.2.0"), "Columns of cards.", bytes, None);
        assert_eq!(entry.wasm, "kanban-viewer-0.2.0.wasm");
        assert_eq!(entry.sig, "kanban-viewer-0.2.0.wasm.sig");
        let index = Index { v: INDEX_FORMAT, plugins: vec![entry.clone()] };
        let parsed = Index::parse(&serde_json::to_vec(&index).unwrap()).unwrap();
        assert_eq!(parsed, index);
        assert!(parsed.names("kanban-viewer-0.2.0.wasm"));
        assert!(parsed.names("kanban-viewer-0.2.0.wasm.sig"));
        assert!(!parsed.names("pimble-0.20.0-linux-x86_64.tar.gz"));
        assert!(!parsed.names("../secrets"));
    }

    #[test]
    fn a_newer_format_is_refused_with_a_sentence() {
        let said = Index::parse(br#"{"v": 2, "plugins": []}"#).unwrap_err();
        assert!(said.contains("newer format"), "{said}");
    }

    #[test]
    fn what_a_row_offers() {
        let entry = entry_for(&manifest("kanban-viewer", "0.2.0"), "", b"x", None);
        assert_eq!(offer(&entry, None, false), Offer::Install);
        assert_eq!(offer(&entry, Some("0.1.0"), true), Offer::Update { from: "0.1.0".into() });
        assert_eq!(offer(&entry, Some("0.2.0"), true), Offer::Installed);
        assert_eq!(offer(&entry, Some("0.3.0"), true), Offer::Installed);
        assert_eq!(offer(&entry, Some("0.1.0"), false), Offer::FromFile);
        let mut later = entry.clone();
        later.abi = ABI_VERSION + 1;
        assert_eq!(offer(&later, None, false), Offer::NeedsNewer);
        assert_eq!(offer(&later, Some("0.1.0"), true), Offer::Installed);
    }

    #[test]
    fn only_a_download_the_key_signed_with_the_listed_hash_verifies() {
        use ed25519_dalek::Signer as _;
        let (key, public) = key_pair();
        let bytes = b"\0asm-the-plugin".to_vec();
        let entry = entry_for(&manifest("kanban-viewer", "0.2.0"), "", &bytes, None);
        let sig = key.sign(&bytes).to_bytes();
        assert!(verify_download(&entry, &public, &bytes, &sig).is_ok());

        let mut tampered = bytes.clone();
        tampered[3] ^= 1;
        assert!(verify_download(&entry, &public, &tampered, &sig).is_err(), "the hash");

        let mut relisted = entry_for(&manifest("kanban-viewer", "0.2.0"), "", &tampered, None);
        relisted.sha256 = sha256_hex(&tampered);
        assert!(verify_download(&relisted, &public, &tampered, &sig).is_err(), "the signature");

        let (_, other) = {
            use base64::Engine as _;
            let k = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
            (k.clone(), base64::engine::general_purpose::STANDARD.encode(k.verifying_key().as_bytes()))
        };
        assert!(verify_download(&entry, &other, &bytes, &sig).is_err(), "another key");
        assert!(verify_download(&entry, "", &bytes, &sig).is_err(), "no key");
    }

    #[test]
    fn a_plugin_must_be_the_one_listed() {
        let entry = entry_for(&manifest("kanban-viewer", "0.2.0"), "", b"x", None);
        assert!(matches(&entry, &manifest("kanban-viewer", "0.2.0")).is_ok());
        assert!(matches(&entry, &manifest("todo-viewer", "0.2.0")).is_err());
        assert!(matches(&entry, &manifest("kanban-viewer", "0.1.0")).is_err());
    }

    #[test]
    fn provenance_round_trips_and_reads_nothing_as_empty() {
        let mut p = Provenance::default();
        p.plugins.insert("kanban-viewer".into(), "0.2.0".into());
        let back = Provenance::parse(&p.to_json());
        assert!(back.from_library("kanban-viewer"));
        assert!(!back.from_library("todo-viewer"));
        assert_eq!(Provenance::parse(b"not json"), Provenance::default());
    }
}

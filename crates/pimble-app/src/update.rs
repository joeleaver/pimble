//! A home-folder install that keeps itself current (docs/UPDATE_CONTRACT.md).
//!
//! `pimble --install` copies the running binaries into the install folder and
//! marks it; a binary running from a marked folder checks the release feed,
//! downloads and verifies a newer package, stages it beside itself, and on
//! "Restart to update" moves it into place and starts it. Nothing here runs for
//! a binary outside a marked folder.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rinch::prelude::*;
use serde::Deserialize;

use crate::state::AppStore;

/// The release feed: the accounts service's view of the latest GitHub release.
const FEED: &str = "https://pimble.app/api/v1/releases";
/// The Ed25519 key release packages are signed with, the raw 32 bytes in base64
/// (`tools/update-key.sh` writes it). Anything that is not such a key refuses
/// every update.
const PUBLIC_KEY: &str = include_str!("../update-key.pub");
/// The file that makes a folder a managed install.
const MARKER: &str = "pimble-install.json";
/// Where an update waits for "Restart to update", inside the install folder.
const STAGE: &str = "update";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(target_os = "linux")]
const OS: &str = "linux";
#[cfg(target_os = "windows")]
const OS: &str = "windows";
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
const OS: &str = "other";

const PACKAGE_SUFFIX: &str = if cfg!(windows) { ".zip" } else { ".tar.gz" };
const EXE: &str = if cfg!(windows) { "pimble.exe" } else { "pimble" };
const MCP: &str = if cfg!(windows) { "pimble-mcp.exe" } else { "pimble-mcp" };

// ── The install ─────────────────────────────────────────────────────────────

/// The folder `--install` puts the app in.
pub fn install_dir() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    Some(if cfg!(windows) { base.join("Programs").join("Pimble") } else { base.join("pimble").join("app") })
}

/// The managed install the running binary is in, if it is in one.
pub fn managed_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let dir = exe.parent()?.to_path_buf();
    dir.join(MARKER).is_file().then_some(dir)
}

/// `pimble --install`: copy this binary and the `pimble-mcp` beside it into
/// [`install_dir`], mark it, and (on Linux) put it on the PATH and in the menu.
pub fn install() -> Result<PathBuf, String> {
    let dir = install_dir().ok_or("no local data folder to install into")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let exe = std::env::current_exe().map_err(|e| format!("finding this program: {e}"))?;
    let from = exe.parent().ok_or("this program has no folder")?;
    let mut copied = vec![EXE];
    copy_over(&exe, &dir.join(EXE))?;
    if from.join(MCP).is_file() {
        copy_over(&from.join(MCP), &dir.join(MCP))?;
        copied.push(MCP);
    }
    write_marker(&dir, CURRENT)?;
    #[cfg(target_os = "linux")]
    desktop_integration(&dir, &copied)?;
    let _ = copied;
    Ok(dir)
}

fn write_marker(dir: &Path, version: &str) -> Result<(), String> {
    let body = serde_json::json!({ "version": version }).to_string();
    std::fs::write(dir.join(MARKER), body).map_err(|e| format!("writing the install marker: {e}"))
}

/// Copy `from` to `to` through a file beside `to`, so a running `to` is never
/// half written.
fn copy_over(from: &Path, to: &Path) -> Result<(), String> {
    let tmp = to.with_extension("incoming");
    std::fs::copy(from, &tmp).map_err(|e| format!("copying {}: {e}", from.display()))?;
    replace_file(&tmp, to)
}

/// Move `new` over `dest`. On Linux a rename: a running `dest` keeps its open
/// file. Windows will not replace a running program, but it will rename one, so
/// the old file steps aside to `*.old` first ([`clean_up`] removes it).
fn replace_file(new: &Path, dest: &Path) -> Result<(), String> {
    if cfg!(windows) && dest.exists() {
        let old = dest.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(dest, &old).map_err(|e| format!("moving {} aside: {e}", dest.display()))?;
    }
    std::fs::rename(new, dest).map_err(|e| format!("putting {} in place: {e}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn desktop_integration(dir: &Path, binaries: &[&str]) -> Result<(), String> {
    let data = dirs::data_local_dir().ok_or("no local data folder")?;
    if let Some(bin) = dirs::executable_dir() {
        std::fs::create_dir_all(&bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
        for name in binaries {
            let link = bin.join(name);
            // Replace a link (an earlier install's); never a file someone put there.
            if link.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                let _ = std::fs::remove_file(&link);
            }
            if !link.exists() {
                std::os::unix::fs::symlink(dir.join(name), &link)
                    .map_err(|e| format!("linking {}: {e}", link.display()))?;
            }
        }
    }
    let icons = data.join("icons").join("hicolor");
    let pngs: [(&str, &[u8]); 5] = [
        ("32x32", include_bytes!("../../../assets/brand/png/pimble-32.png")),
        ("48x48", include_bytes!("../../../assets/brand/png/pimble-48.png")),
        ("64x64", include_bytes!("../../../assets/brand/png/pimble-64.png")),
        ("128x128", include_bytes!("../../../assets/brand/png/pimble-128.png")),
        ("256x256", include_bytes!("../../../assets/brand/png/pimble-256.png")),
    ];
    for (size, bytes) in pngs {
        let at = icons.join(size).join("apps");
        std::fs::create_dir_all(&at).map_err(|e| format!("creating {}: {e}", at.display()))?;
        std::fs::write(at.join("pimble.png"), bytes).map_err(|e| format!("writing an icon: {e}"))?;
    }
    let scalable = icons.join("scalable").join("apps");
    std::fs::create_dir_all(&scalable).map_err(|e| format!("creating {}: {e}", scalable.display()))?;
    std::fs::write(scalable.join("pimble.svg"), include_bytes!("../../../assets/brand/pimble-icon.svg"))
        .map_err(|e| format!("writing an icon: {e}"))?;
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| format!("creating {}: {e}", apps.display()))?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Pimble\nComment=Your notes, on your disk, shared when you choose\n\
         Exec={}\nIcon=pimble\nTerminal=false\nCategories=Office;Utility;\nStartupWMClass=pimble\n",
        dir.join(EXE).display()
    );
    std::fs::write(apps.join("pimble.desktop"), entry).map_err(|e| format!("writing the menu entry: {e}"))
}

// ── At start ────────────────────────────────────────────────────────────────

/// Before anything else in a process started by "Restart to update": wait for
/// the process it replaces to exit, so the two never hold the server's port at
/// once. Then tidy what an earlier update left.
pub fn at_start() {
    if let Some(pid) = std::env::var("PIMBLE_WAIT_FOR_PID").ok().and_then(|p| p.parse::<u32>().ok()) {
        std::env::remove_var("PIMBLE_WAIT_FOR_PID");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while process_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if let Some(dir) = managed_dir() {
        clean_up(&dir);
    }
}

fn process_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    } else {
        false
    }
}

fn clean_up(dir: &Path) {
    for name in [EXE, MCP] {
        let _ = std::fs::remove_file(dir.join(name).with_extension("old"));
    }
    // A staged update this version has already reached (installed by hand, say).
    if staged_version(dir).is_some_and(|v| !newer_than_current(&v)) {
        let _ = std::fs::remove_dir_all(dir.join(STAGE));
    }
}

// ── The check ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Feed {
    version: String,
    assets: Vec<FeedAsset>,
}

#[derive(Deserialize)]
struct FeedAsset {
    name: String,
    os: String,
    url: String,
    #[serde(default)]
    signature_url: Option<String>,
}

fn newer_than_current(version: &str) -> bool {
    let parse = |v: &str| semver::Version::parse(v.trim_start_matches('v')).ok();
    match (parse(version), parse(CURRENT)) {
        (Some(v), Some(current)) => v > current,
        _ => false,
    }
}

fn staged_version(dir: &Path) -> Option<String> {
    let stage = dir.join(STAGE);
    let ready: serde_json::Value = serde_json::from_slice(&std::fs::read(stage.join("ready.json")).ok()?).ok()?;
    let version = ready.get("version")?.as_str()?.to_string();
    stage.join(EXE).is_file().then_some(version)
}

/// Check the feed once; stage a newer verified package. Answers the version
/// that is staged, if one is.
fn check_and_stage(dir: &Path) -> Result<Option<String>, String> {
    let feed_url = std::env::var("PIMBLE_UPDATE_FEED").unwrap_or_else(|_| FEED.to_string());
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("pimble/{CURRENT}"))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let get = |url: &str| -> Result<Vec<u8>, String> {
        let resp = client.get(url).send().and_then(|r| r.error_for_status()).map_err(|e| format!("{url}: {e}"))?;
        resp.bytes().map(|b| b.to_vec()).map_err(|e| format!("{url}: {e}"))
    };
    let feed: Feed = serde_json::from_slice(&get(&feed_url)?).map_err(|e| format!("reading the feed: {e}"))?;
    let version = feed.version.trim_start_matches('v').to_string();
    if !newer_than_current(&version) {
        return Ok(None);
    }
    if staged_version(dir).as_deref() == Some(version.as_str()) {
        return Ok(Some(version));
    }
    let asset = feed
        .assets
        .iter()
        .find(|a| a.os == OS && a.name.ends_with(PACKAGE_SUFFIX))
        .ok_or_else(|| format!("{version} has no {OS} package"))?;
    let package = get(&asset.url)?;
    let signature = get(&asset.signature_url.clone().unwrap_or_else(|| format!("{}.sig", asset.url)))?;
    verify(&package, &signature)?;

    let stage = dir.join(STAGE);
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("creating {}: {e}", stage.display()))?;
    let archive = stage.join(format!("package{PACKAGE_SUFFIX}"));
    std::fs::write(&archive, &package).map_err(|e| format!("saving the package: {e}"))?;
    let unpacked = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&stage)
        .status()
        .map_err(|e| format!("running tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !unpacked.success() || !stage.join(EXE).is_file() {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(format!("the {version} package did not unpack to a {EXE}"));
    }
    std::fs::write(stage.join("ready.json"), serde_json::json!({ "version": version }).to_string())
        .map_err(|e| format!("marking the update ready: {e}"))?;
    Ok(Some(version))
}

/// Does `signature` sign `package` with the compiled-in key?
fn verify(package: &[u8], signature: &[u8]) -> Result<(), String> {
    verify_with(PUBLIC_KEY, package, signature)
}

fn verify_with(public_key: &str, package: &[u8], signature: &[u8]) -> Result<(), String> {
    use base64::Engine as _;
    use ed25519_dalek::{Signature, VerifyingKey};
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(public_key.trim())
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or("this build has no update key (tools/update-key.sh)")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|e| format!("the update key: {e}"))?;
    let signature = Signature::from_slice(signature).map_err(|e| format!("the signature: {e}"))?;
    key.verify_strict(package, &signature).map_err(|_| "the package's signature does not verify".to_string())
}

// ── The app's side ──────────────────────────────────────────────────────────

thread_local! {
    static STORE: Cell<Option<AppStore>> = const { Cell::new(None) };
}
/// A version the checker staged, handed to the main thread.
static STAGED: Mutex<Option<String>> = Mutex::new(None);

/// Offer a staged update at once, and check in the background from now on.
/// Does nothing outside a managed install.
pub fn start(store: AppStore) {
    let Some(dir) = managed_dir() else { return };
    STORE.with(|s| s.set(Some(store)));
    if let Some(version) = staged_version(&dir).filter(|v| newer_than_current(v)) {
        store.update_ready.set(Some(version));
    }
    std::thread::Builder::new()
        .name("pimble-update".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK);
            loop {
                match check_and_stage(&dir) {
                    Ok(Some(version)) => {
                        *STAGED.lock().unwrap() = Some(version);
                        run_on_main_thread(deliver);
                    }
                    Ok(None) => tracing::info!("update check: {CURRENT} is the latest"),
                    Err(e) => tracing::warn!("update check: {e}"),
                }
                std::thread::sleep(CHECK_EVERY);
            }
        })
        .ok();
}

fn deliver() {
    let Some(version) = STAGED.lock().unwrap().take() else { return };
    if let Some(store) = STORE.with(Cell::get) {
        tracing::info!("update {version} is ready");
        store.update_ready.set(Some(version));
    }
}

/// "Restart to update": move the staged binaries into place, start the new
/// one, and close this window.
pub fn restart_to_update(store: AppStore) {
    match apply(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => close_current_window(),
        Err(e) => {
            tracing::warn!("restart to update: {e}");
            store.update_ready.set(None);
            store.notice.set(format!("The update could not be installed: {e}"));
        }
    }
}

fn apply(args: &[String]) -> Result<(), String> {
    let dir = managed_dir().ok_or("this copy of Pimble is not an installed one")?;
    let version = staged_version(&dir).ok_or("no update is ready")?;
    let stage = dir.join(STAGE);
    for name in [EXE, MCP] {
        if stage.join(name).is_file() {
            replace_file(&stage.join(name), &dir.join(name))?;
        }
    }
    write_marker(&dir, &version)?;
    let _ = std::fs::remove_dir_all(&stage);
    std::process::Command::new(dir.join(EXE))
        .args(args)
        .env("PIMBLE_WAIT_FOR_PID", std::process::id().to_string())
        .spawn()
        .map_err(|e| format!("starting {version}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key_pair() -> (SigningKey, String) {
        use base64::Engine as _;
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let public = base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_bytes());
        (signing, public)
    }

    #[test]
    fn a_package_signed_with_the_key_verifies_and_nothing_else_does() {
        let (signing, public) = key_pair();
        let package = b"pimble 1.2.3";
        let signature = signing.sign(package).to_bytes();
        assert!(verify_with(&public, package, &signature).is_ok());
        assert!(verify_with(&public, b"pimble 1.2.4", &signature).is_err());
        let other = SigningKey::from_bytes(&[8u8; 32]).sign(package).to_bytes();
        assert!(verify_with(&public, package, &other).is_err());
        assert!(verify_with(&public, package, &signature[..63]).is_err());
    }

    #[test]
    fn a_build_without_a_key_refuses_every_update() {
        let (signing, _) = key_pair();
        let signature = signing.sign(b"x").to_bytes();
        let refused = verify_with("not a key", b"x", &signature).unwrap_err();
        assert!(refused.contains("no update key"), "{refused}");
    }

    #[test]
    fn only_a_later_version_is_an_update() {
        assert!(newer_than_current("v999.0.0"));
        assert!(newer_than_current("999.0.0"));
        assert!(!newer_than_current(CURRENT));
        assert!(!newer_than_current("0.0.1"));
        assert!(!newer_than_current("garbage"));
    }

    #[test]
    fn the_feed_reads_with_and_without_signature_urls() {
        let feed: Feed = serde_json::from_str(
            r#"{"version":"v1.0.0","published_at":null,"assets":[
                {"name":"a.tar.gz","os":"linux","url":"u","size":1,"signature_url":"s"},
                {"name":"b.zip","os":"windows","url":"v","size":1}]}"#,
        )
        .unwrap();
        assert_eq!(feed.assets[0].signature_url.as_deref(), Some("s"));
        assert_eq!(feed.assets[1].signature_url, None);
    }
}

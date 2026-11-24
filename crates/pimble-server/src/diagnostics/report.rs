//! Help > "Send Diagnostics..." on the desktop: the last two runs' logs of the
//! app and of an MCP-run server, sent with the send key.

use std::fs;
use std::io::Write as _;
use std::time::{Duration, SystemTime};

use pimble_core::diagnostics::{random_hex, timestamp, Envelope, Scrubber};
use serde_json::Value;

use super::native::{ids, install_id, queue_dir, send_key, ACCOUNT, DEFAULT_URL, RECORDER};

/// The most of one log a report carries: its last 8 MB.
const REPORT_LOG_BYTES: usize = 8 << 20;

/// Send a report (docs/DIAGNOSTICS_CONTRACT.md section 4): the last two runs'
/// logs of the app and of an MCP-run server, scrubbed as records are, then
/// `more_logs` (`(name, text)`, what a client of this server hands over), the
/// open stores as the window knows them (`stores`), and the person's `note`.
/// Sent whether or not diagnostics are on: pressing Send is the asking. The
/// answer is the report's code, for the person to tell us. Blocking: call it
/// off the UI thread.
pub fn send_report(note: &str, stores: Value, more_logs: Vec<(String, String)>) -> Result<String, String> {
    let key = send_key().ok_or_else(|| "This copy of Pimble cannot send diagnostics: it was built without the key (a development build).".to_string())?;
    let dir = queue_dir().ok_or_else(|| "There is no folder to keep this computer's install number in.".to_string())?;
    let _ = fs::create_dir_all(&dir);
    let envelope = Envelope {
        install: install_id(&dir.join("install-id")),
        session: ids().map(|(_, s)| s).unwrap_or_else(|| random_hex(16)),
        app: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
    };
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let scrubber = RECORDER.get().and_then(|r| r.scrubber.read().ok().map(|s| s.clone())).unwrap_or_else(|| Scrubber::new(home.as_deref()));
    let mut logs = Vec::new();
    if let Some(logs_dir) = crate::logs::dir() {
        for name in ["pimble.log", "pimble.previous.log", "server.log", "server.previous.log"] {
            let Ok(bytes) = fs::read(logs_dir.join(name)) else { continue };
            let start = bytes.len().saturating_sub(REPORT_LOG_BYTES);
            logs.push((name.to_string(), String::from_utf8_lossy(&bytes[start..]).into_owned()));
        }
    }
    logs.extend(more_logs);
    let account = ACCOUNT.read().ok().and_then(|a| a.clone());
    let body = envelope.report(&scrubber, &timestamp(SystemTime::now().into()), note, logs, stores, account.as_deref());
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let gz = gz.finish().map_err(|e| e.to_string())?;
    let base = std::env::var("PIMBLE_DIAGNOSTICS_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
        let answer = client
            .post(format!("{base}/v1/reports"))
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(gz)
            .send()
            .await
            .map_err(|e| format!("Pimble's diagnostics server could not be reached ({e}). Try again when this computer is online."))?;
        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Pimble's diagnostics server did not take the report ({status}: {}).", text.trim()));
        }
        pimble_core::diagnostics::report_code(&text).ok_or_else(|| "Pimble's diagnostics server answered without a report code.".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a diagnostics server (`diagnostics/dev.sh`):
    /// `PIMBLE_DIAGNOSTICS_URL=http://127.0.0.1:8788 PIMBLE_DIAGNOSTICS_KEY=dev-key
    /// XDG_DATA_HOME=<scratch> cargo test -p pimble-server --lib report_round_trip -- --ignored`.
    /// Point `XDG_DATA_HOME` somewhere scratch: the report reads the logs there.
    #[test]
    #[ignore = "needs a diagnostics server"]
    fn report_round_trip() {
        let logs = crate::logs::dir().unwrap();
        fs::create_dir_all(&logs).unwrap();
        let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();
        fs::write(logs.join("pimble.log"), format!("INFO opened '{home}/Notes.pimble' for tester@example.com\nWARN something odd\n")).unwrap();
        let code = send_report("The share stopped syncing.", serde_json::json!({ "stores": [{ "name": "Notes" }] }), Vec::new()).expect("a report code");
        assert_eq!(code.len(), 8, "{code}");
        eprintln!("REPORT {code}");
    }
}

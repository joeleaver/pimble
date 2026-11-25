//! Diagnostics through a server (docs/DIAGNOSTICS_CONTRACT.md,
//! docs/ONE_CODEBASE_CONTRACT.md section 6 item 10): what a client that does
//! not record for itself tells the server it uses, and Help > "Send
//! Diagnostics..." sent by that server. The browser's page is that client;
//! the desktop app records and sends in its own process as well.

use serde::{Deserialize, Serialize};

/// `setDiagnostics`: whether the person has diagnostics on, and the install
/// number the client keeps (the browser page's, which outlives a browser that
/// is not remembered); the server records under it. `clear_owed`: the client
/// noted an off that no server has confirmed clearing for (the browser's
/// worker can end before it could write the off down), so what waits from
/// before it must go before anything is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetDiagnosticsRequest {
    pub enabled: bool,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub clear_owed: bool,
}

/// `setDiagnostics`' answer: whether every clear the request said was owed
/// has run (always, when it said none), so the client may drop its note.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetDiagnosticsResponse {
    #[serde(default)]
    pub cleared: bool,
}

/// One log a client hands over for a report, as `(name, text)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsLog {
    pub name: String,
    pub text: String,
}

/// `sendDiagnostics`: the person's note, the stores as the window knows them,
/// and the client's own logs; the server adds its own log and sends it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendDiagnosticsRequest {
    pub note: String,
    pub stores: serde_json::Value,
    #[serde(default)]
    pub logs: Vec<DiagnosticsLog>,
}

/// The report's code, for the person to tell us.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendDiagnosticsResponse {
    pub report: String,
}

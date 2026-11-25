//! `setDiagnostics` and `sendDiagnostics` (docs/ONE_CODEBASE_CONTRACT.md,
//! "Built: wave 5, part 2"): the person's diagnostics choice and Help >
//! "Send Diagnostics...", answered by whichever server the app runs on. On
//! the desktop they are `crate::diagnostics`' saved settings and report; in a
//! browser's worker, the worker's recorder (`crate::diagnostics::worker`),
//! which sends through the accounts service as the signed-in account.

use jsonrpsee::types::ErrorObjectOwned;
use pimble_rpc::{to_rpc_error, SendDiagnosticsRequest, SendDiagnosticsResponse, SetDiagnosticsRequest};

use super::RpcHandler;

impl RpcHandler {
    /// On or off. On the desktop the saved choice (keeping whether the notice
    /// was shown): off deletes what waits at once, on starts recording the
    /// next time a program starts. In a worker, from now on, after any clear
    /// the page says is owed. Whether every owed clear has run: on the
    /// desktop, where an off deletes at once and no client keeps a note, it
    /// always has.
    pub(super) async fn diagnostics_set(&self, request: SetDiagnosticsRequest) -> Result<bool, ErrorObjectOwned> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (request.install, request.clear_owed);
            let settings = crate::diagnostics::Settings { enabled: request.enabled, ..crate::diagnostics::settings() };
            self.runtime().run_heavy(move || crate::diagnostics::save_settings(&settings)).await.map_err(to_rpc_error)?.map_err(to_rpc_error)?;
            Ok(true)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(crate::diagnostics::configure(self, request.enabled, request.install, request.clear_owed).await)
        }
    }

    /// One report: the client's logs, this server's, the note and the stores;
    /// the report's code, or a sentence saying why it was not sent.
    pub(super) async fn diagnostics_send(&self, request: SendDiagnosticsRequest) -> Result<SendDiagnosticsResponse, ErrorObjectOwned> {
        let logs: Vec<(String, String)> = request.logs.into_iter().map(|log| (log.name, log.text)).collect();
        #[cfg(not(target_arch = "wasm32"))]
        let report = {
            let SendDiagnosticsRequest { note, stores, .. } = request;
            self.runtime().run_heavy(move || crate::diagnostics::send_report(&note, stores, logs)).await.map_err(to_rpc_error)?
        };
        #[cfg(target_arch = "wasm32")]
        let report = crate::diagnostics::send_report(self, &request.note, request.stores, logs).await;
        report.map(|report| SendDiagnosticsResponse { report }).map_err(to_rpc_error)
    }
}

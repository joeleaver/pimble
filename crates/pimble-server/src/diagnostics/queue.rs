//! The desktop's queue and its uploader: NDJSON files in the queue folder,
//! closed after [`FILE_AGE`] or [`FILE_BYTES`], sent every minute by whichever
//! program claims them first.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::native::{Msg, ENABLED, MAX_QUEUED_BYTES};

const FILE_BYTES: u64 = 256 * 1024;
const FILE_AGE: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

pub(super) struct Writer {
    pub(super) dir: PathBuf,
    pub(super) session: String,
    pub(super) open: Option<PathBuf>,
    pub(super) opened: Instant,
    pub(super) uploader: Uploader,
}

impl Writer {
    pub(super) fn run(mut self, rx: Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Msg::Line(line)) => self.append(&line),
                Ok(Msg::Flush(done)) => {
                    self.close();
                    self.uploader.send_waiting(Some(Duration::from_secs(3)));
                    let _ = done.send(());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if !ENABLED.load(Ordering::Relaxed) {
                return;
            }
            if self.open.is_some() && self.opened.elapsed() >= FILE_AGE {
                self.close();
            }
            if self.uploader.due() {
                self.uploader.send_waiting(None);
            }
        }
    }

    fn append(&mut self, line: &str) {
        let session = self.session.clone();
        let dir = self.dir.clone();
        let path = self.open.get_or_insert_with(|| {
            self.opened = Instant::now();
            dir.join(format!("open-{session}.ndjson"))
        });
        let written = OpenOptions::new().create(true).append(true).open(&*path).and_then(|mut f| writeln!(f, "{line}"));
        if written.is_ok() && fs::metadata(&*path).is_ok_and(|m| m.len() >= FILE_BYTES) {
            self.close();
        }
    }

    fn close(&mut self) {
        let Some(open) = self.open.take() else { return };
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let _ = fs::rename(&open, self.dir.join(format!("records-{millis}-{}.ndjson", self.session)));
        cap(&self.dir, MAX_QUEUED_BYTES);
    }
}

/// The oldest closed files go first once more than `max` wait.
fn cap(dir: &Path, max: u64) {
    let mut files = waiting(dir);
    let mut total: u64 = files.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    files.sort();
    for path in files {
        if total <= max {
            break;
        }
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// A file a program claimed to send and never finished (it crashed or was
/// killed mid-send) goes back to the queue after [`STRANDED`].
const STRANDED: Duration = Duration::from_secs(10 * 60);

fn reclaim(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `sending-<32 hex>-records-....ndjson`
        let Some(rest) = name.strip_prefix("sending-").and_then(|r| r.get(33..)) else { continue };
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= STRANDED);
        if old && rest.starts_with("records-") {
            let _ = fs::rename(entry.path(), dir.join(rest));
        }
    }
}

/// The closed files, oldest first (their names start with the time).
fn waiting(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("records-") && n.ends_with(".ndjson")
        }))
        .collect();
    files.sort();
    files
}

pub(super) struct Uploader {
    base: String,
    key: String,
    session: String,
    dir: PathBuf,
    next: Instant,
    backoff: Duration,
    runtime: Option<tokio::runtime::Runtime>,
    client: reqwest::Client,
}

enum Sent {
    Done,
    Later,
}

impl Uploader {
    pub(super) fn new(base: String, key: String, session: String, dir: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default();
        // The first send soon after start, for what an earlier run left.
        Uploader { base, key, session, dir, next: Instant::now() + Duration::from_secs(10), backoff: EVERY, runtime, client }
    }

    fn due(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Send every closed file. Several programs share the queue (the app and
    /// each `pimble-mcp`), so a file is first renamed to this session's
    /// `sending-` name, which only one of them can do.
    fn send_waiting(&mut self, budget: Option<Duration>) {
        reclaim(&self.dir);
        let started = Instant::now();
        let mut ok = true;
        for path in waiting(&self.dir) {
            if budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let claimed = self.dir.join(format!("sending-{}-{name}", self.session));
            if fs::rename(&path, &claimed).is_err() {
                continue;
            }
            match self.send(&claimed) {
                Sent::Done => {
                    let _ = fs::remove_file(&claimed);
                }
                Sent::Later => {
                    let _ = fs::rename(&claimed, &path);
                    ok = false;
                    break;
                }
            }
        }
        self.backoff = if ok { EVERY } else { (self.backoff * 2).min(MAX_BACKOFF) };
        self.next = Instant::now() + self.backoff;
    }

    fn send(&self, path: &Path) -> Sent {
        let Ok(body) = fs::read(path) else { return Sent::Done };
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        if gz.write_all(&body).is_err() {
            return Sent::Done;
        }
        let Ok(gz) = gz.finish() else { return Sent::Done };
        let Some(runtime) = &self.runtime else { return Sent::Later };
        let request = self
            .client
            .post(format!("{}/v1/records", self.base))
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/x-ndjson")
            .header("Content-Encoding", "gzip")
            .body(gz);
        // The request is made inside the runtime: its timeout is a tokio
        // timer, which panics when made outside one.
        match runtime.block_on(async move { request.send().await }) {
            Ok(r) if r.status().is_success() || matches!(r.status().as_u16(), 400 | 413) => Sent::Done,
            Ok(r) => {
                tracing::debug!("diagnostics upload refused ({}); trying later", r.status());
                Sent::Later
            }
            Err(e) => {
                tracing::debug!("diagnostics upload failed: {e}");
                Sent::Later
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_left_mid_send_goes_back_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("sending-{}-records-001-s.ndjson", "a".repeat(32));
        let path = dir.path().join(&name);
        fs::write(&path, b"x").unwrap();
        reclaim(dir.path());
        assert!(path.exists(), "a send in progress is left alone");
        let old = SystemTime::now() - STRANDED - Duration::from_secs(1);
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        reclaim(dir.path());
        assert!(dir.path().join("records-001-s.ndjson").exists());
    }

    #[test]
    fn the_queue_drops_the_oldest_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("records-{i:03}-s.ndjson")), vec![b'x'; 100]).unwrap();
        }
        cap(dir.path(), 250);
        let left: Vec<String> = waiting(dir.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["records-003-s.ndjson", "records-004-s.ndjson"]);
    }
}


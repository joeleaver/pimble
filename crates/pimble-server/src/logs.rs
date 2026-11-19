//! The log files of the programs on a tester's computer
//! (docs/DIAGNOSTICS_CONTRACT.md section 1): `<data dir>/pimble/logs/<name>.log`,
//! begun afresh at each start with the last run's kept as
//! `<name>.previous.log`, and moved aside to `<name>.older.log` whenever it
//! passes [`MAX_BYTES`], so a long run keeps its last 20-40 MB. The app writes
//! `pimble.log` on every platform (before 0.28.3, on Windows only), a server
//! `pimble-mcp` runs writes `server.log`. Standard error gets every line too,
//! where there is one.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Past this a log file is moved aside and begun again.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: std::fs::File,
    written: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Where the logs live.
pub fn dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("pimble").join("logs"))
}

/// `<logs>/<name>.log`.
pub fn path(name: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{name}.log")))
}

/// Start writing every log line to `<name>.log` too (once a process: a second
/// call keeps the first file).
pub fn open(name: &str) {
    let Some(path) = path(name) else { return };
    let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if sink.is_some() {
        return;
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let _ = std::fs::rename(&path, path.with_file_name(format!("{name}.previous.log")));
    if let Ok(file) = std::fs::File::create(&path) {
        *sink = Some(Sink { path, file, written: 0 });
    }
}

fn write_file(buf: &[u8]) {
    let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = sink.as_mut() else { return };
    if s.written + buf.len() as u64 > MAX_BYTES {
        let stem = s.path.file_stem().and_then(|n| n.to_str()).unwrap_or("pimble").to_string();
        let older = s.path.with_file_name(format!("{stem}.older.log"));
        let _ = s.file.flush();
        let _ = std::fs::rename(&s.path, older);
        match std::fs::File::create(&s.path) {
            Ok(file) => {
                s.file = file;
                s.written = 0;
            }
            Err(_) => {
                *sink = None;
                return;
            }
        }
    }
    if s.file.write_all(buf).is_ok() {
        s.written += buf.len() as u64;
    }
}

/// A `tracing_subscriber` writer: the log file once [`open`]ed, and standard
/// error unless `quiet` (a Windows window program has none).
#[derive(Clone, Copy, Default)]
pub struct LogWriter {
    pub quiet: bool,
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        write_file(buf);
        if self.quiet {
            Ok(buf.len())
        } else {
            std::io::stderr().write(buf)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(s) = SINK.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = s.file.flush();
        }
        if self.quiet { Ok(()) } else { std::io::stderr().flush() }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

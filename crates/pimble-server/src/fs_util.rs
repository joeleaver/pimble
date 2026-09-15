//! Small filesystem helper shared by [`crate::auth`]'s server token file and
//! [`crate::credentials`]'s credentials file: both are small, sensitive,
//! single-writer files that must never be world- or group-readable and must
//! never appear half-written to a concurrent reader.

use std::io;
use std::path::{Path, PathBuf};

/// Write `bytes` to `path` atomically (a sibling `.tmp` file, written and
/// `rename`d over `path`) with mode `0600`, creating the parent directory
/// first if it doesn't exist. Synchronous; a caller on an async task should
/// run it via `spawn_blocking`.
pub fn write_atomic_0600(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp_path = PathBuf::from(tmp);

    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // On unix, set the mode the tmp file is *created* with (the `mode`
        // argument to `open()` is only honored when `O_CREAT` actually
        // creates the file), so it is never briefly world- or
        // group-readable at the default umask before the chmod below runs.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(bytes)?;
    }

    // Belt and suspenders: also reassert 0600 after writing, in case a
    // stale `.tmp` file with wider permissions (e.g. left over from before
    // this fix) already existed — `open()`'s `mode` only applies to a file
    // it actually creates, not one it reuses.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
    }

    std::fs::rename(&tmp_path, path)
}

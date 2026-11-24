//! What the computer is, and runs that ended without quitting.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::native::{flush, record};

/// What the computer is, at start.
pub(super) fn record_system(program: &'static str) {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    record(
        "system",
        json!({
            "program": program,
            "os_version": os_version(),
            "arch": std::env::consts::ARCH,
            "cores": cores,
            "ram_mb": ram_mb(),
            "host": crate::local::host_kind(),
        }),
    );
}

fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = fs::read_to_string("/etc/os-release") {
            if let Some(line) = text.lines().find(|l| l.starts_with("PRETTY_NAME=")) {
                return line.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string();
            }
        }
        String::new()
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").args(["/C", "ver"]).creation_flags(0x0800_0000).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        String::new()
    }
}

fn ram_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|kb| kb.parse::<u64>().ok()))
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// A run that ended without quitting shows at the next start (its `running`
/// file is still there), and a panic on any thread is recorded and sent at
/// once.
pub(super) fn crash_watch(dir: &Path, program: &'static str, session: &str) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(last) = name.strip_prefix("running-") else { continue };
            let info = fs::read_to_string(entry.path()).unwrap_or_default();
            let (pid, was) = info.split_once(' ').unwrap_or(("", ""));
            // Another program of this computer that is still running (the
            // app beside an MCP) is no crash.
            if pid.parse::<u32>().is_ok_and(process_alive) {
                continue;
            }
            record("previous_run", json!({"session": last, "program": was, "message": "the last run ended without quitting (a crash, a kill, or the computer going off)"}));
            let _ = fs::remove_file(entry.path());
        }
    }
    let _ = fs::write(dir.join(format!("running-{session}")), format!("{} {program}", std::process::id()));
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "(not a string)".into());
        let backtrace: String = std::backtrace::Backtrace::force_capture().to_string().chars().take(6000).collect();
        record("panic", json!({"message": message, "thread": thread, "location": location, "backtrace": backtrace}));
        if thread != "pimble-diagnostics" {
            flush(Duration::from_secs(3));
        }
        earlier(info);
    }));
}

fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(target_os = "windows")]
    {
        // `tasklist` names the process when it runs; no console flashes up.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = pid;
        false
    }
}

//! What every recorder of diagnostics does alike (docs/DIAGNOSTICS_CONTRACT.md
//! sections 2-5), whatever it runs in: the desktop's programs, the server in a
//! browser's worker, and the browser's page. A record's envelope, which log
//! lines are recorded and how often, and a report's body. Nothing here does
//! I/O: where records wait and how they are sent is each host's.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::{json, Value};

use super::Scrubber;

/// The version of the record envelope.
pub const VERSION: u32 = 1;
/// Warnings and errors recorded per session; the `info` lines of the sync
/// and sharing targets have a budget of their own.
pub const MAX_WARNINGS: u32 = 500;
pub const MAX_INFO: u32 = 2000;
/// The same message (numbers aside) is recorded this often, then at its
/// 10th, 100th, 1000th... time with the count.
pub const SAME_MESSAGE: u32 = 3;

/// The targets whose `info` lines are recorded wherever they run: how syncing
/// and sharing go, and what the app makes of it.
pub const INFO_TARGETS: &[&str] = &[
    "pimble_server::vault_link",
    "pimble_server::sync_link",
    "pimble_server::share",
    "pimble_server::relay_face",
    "pimble_server::relay_tunnel",
    "pimble_server::handler",
    "pimble_server::local",
    "pimble_server::server",
    "pimble_app::events",
];

/// Whether the `n`th sighting of one message is recorded.
pub fn recorded(n: u32) -> bool {
    n <= SAME_MESSAGE || (n >= 10 && {
        let mut m = n;
        while m >= 10 && m % 10 == 0 {
            m /= 10;
        }
        m == 1
    })
}

/// Whether `target` is one of `targets` or inside one of them.
pub fn is_info_target(target: &str, targets: &[&str]) -> bool {
    targets.iter().any(|t| target == *t || target.strip_prefix(t).is_some_and(|rest| rest.starts_with("::")))
}

/// Every string in `value`, at every depth, scrubbed.
pub fn scrub_value(s: &Scrubber, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(s.scrub(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| scrub_value(s, v)).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, scrub_value(s, v))).collect()),
        other => other,
    }
}

/// How serious a log line is, as far as recording goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// `warn` and `error`: recorded from every target.
    Warning,
    /// `info`: recorded from the info targets only.
    Info,
}

/// What one session has recorded of its log: how often each message was
/// seen, and how much of each budget is spent.
#[derive(Debug, Default)]
pub struct LogBudget {
    seen: HashMap<String, u32>,
    warnings: u32,
    infos: u32,
}

impl LogBudget {
    /// The message to record for this line, with its count once it repeats,
    /// or `None` when it is not recorded (seen too often, or its budget
    /// spent). The caller decides first whether the line is one that is
    /// recorded at all ([`Severity`], [`is_info_target`]).
    pub fn admit(&mut self, severity: Severity, target: &str, message: &str) -> Option<String> {
        let key = format!("{target} {}", message.chars().filter(|c| !c.is_ascii_digit()).collect::<String>());
        let n = self.seen.entry(key).or_insert(0);
        *n += 1;
        let n = *n;
        if !recorded(n) {
            return None;
        }
        let (count, max) = match severity {
            Severity::Warning => (&mut self.warnings, MAX_WARNINGS),
            Severity::Info => (&mut self.infos, MAX_INFO),
        };
        *count += 1;
        if *count > max {
            return None;
        }
        let mut text = message.to_string();
        if n > SAME_MESSAGE {
            let _ = write!(text, " (seen {n} times)");
        }
        Some(text)
    }
}

/// Who a recorder is: this install (one per computer, or per browser), this
/// run of the program, the app's version and the operating system's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub install: String,
    pub session: String,
    pub app: String,
    pub os: String,
}

impl Envelope {
    /// One record line (`ts` RFC 3339), `data`'s strings scrubbed.
    pub fn record(&self, scrubber: &Scrubber, seq: u64, ts: &str, kind: &str, data: Value, account: Option<&str>) -> String {
        let mut line = json!({
            "v": VERSION,
            "install": self.install,
            "session": self.session,
            "seq": seq,
            "ts": ts,
            "app": self.app,
            "os": self.os,
            "kind": kind,
            "data": scrub_value(scrubber, data),
        });
        if let Some(account) = account {
            line["account"] = Value::String(account.to_string());
        }
        line.to_string()
    }

    /// A report's body (Help > "Send Diagnostics..."): the person's note and
    /// the stores the window knows, scrubbed, and `logs`, each `(name, text)`
    /// with its lines scrubbed.
    pub fn report(&self, scrubber: &Scrubber, ts: &str, note: &str, logs: Vec<(String, String)>, stores: Value, account: Option<&str>) -> Value {
        let logs: Vec<Value> = logs
            .into_iter()
            .map(|(name, text)| {
                let text: String = text.lines().map(|line| scrubber.scrub(line)).collect::<Vec<_>>().join("\n");
                json!({ "name": name, "text": text })
            })
            .collect();
        let mut body = json!({
            "v": VERSION,
            "install": self.install,
            "session": self.session,
            "app": self.app,
            "os": self.os,
            "ts": ts,
            "note": scrubber.scrub(note),
            "logs": logs,
            "stores": scrub_value(scrubber, stores),
        });
        if let Some(account) = account {
            body["account"] = Value::String(account.to_string());
        }
        body
    }
}

/// `bytes` random bytes as lowercase hex: an install's or a session's id.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    // The system's generator; it does not fail on any platform Pimble runs on.
    let _ = getrandom::getrandom(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// An install id as kept: 32 hex digits, or nothing.
pub fn valid_install(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A report's code, from the diagnostics server's answer.
pub fn report_code(answer: &str) -> Option<String> {
    serde_json::from_str::<Value>(answer).ok().and_then(|v| v["report"].as_str().map(str::to_string))
}

/// A record's time.
pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_message_is_recorded_three_times_then_at_tens() {
        let kept: Vec<u32> = (1..=1000).filter(|&n| recorded(n)).collect();
        assert_eq!(kept, [1, 2, 3, 10, 100, 1000]);
    }

    #[test]
    fn info_targets_match_whole_module_paths() {
        assert!(is_info_target("pimble_server::vault_link", INFO_TARGETS));
        assert!(is_info_target("pimble_server::sync_link::reconcile", INFO_TARGETS));
        assert!(!is_info_target("pimble_server::sync_linkage", INFO_TARGETS));
        assert!(!is_info_target("pimble_plugin_host::registry", INFO_TARGETS));
    }

    #[test]
    fn strings_in_a_record_are_scrubbed_at_every_depth() {
        let s = Scrubber::new(Some("/home/tester"));
        let v = scrub_value(&s, json!({"message": "at /home/tester/x for a@b.co", "list": ["/home/tester/y"], "n": 3}));
        assert_eq!(v, json!({"message": "at ~/x for <email>", "list": ["~/y"], "n": 3}));
    }

    #[test]
    fn a_budget_counts_repeats_and_stops_at_its_limit() {
        let mut budget = LogBudget::default();
        let said: Vec<Option<String>> = (0..11).map(|i| budget.admit(Severity::Warning, "t", &format!("retry {i}"))).collect();
        assert_eq!(said[0].as_deref(), Some("retry 0"));
        assert_eq!(said[2].as_deref(), Some("retry 2"));
        assert!(said[3].is_none() && said[8].is_none());
        assert_eq!(said[9].as_deref(), Some("retry 9 (seen 10 times)"));
        // Distinct lines (the target keeps its digits): only the budget stops them.
        let mut budget = LogBudget::default();
        let kept = (0..MAX_INFO + 10).filter(|i| budget.admit(Severity::Info, &format!("t{i}"), "synced").is_some()).count();
        assert_eq!(kept as u32, MAX_INFO);
    }

    #[test]
    fn a_record_carries_its_envelope_and_the_account() {
        let e = Envelope { install: "i".into(), session: "s".into(), app: "1.0".into(), os: "web".into() };
        let s = Scrubber::new(None);
        let line: Value = serde_json::from_str(&e.record(&s, 7, "2026-11-23T00:00:00.000Z", "log", json!({"message": "to a@b.co"}), Some("u1"))).unwrap();
        assert_eq!(line["seq"], 7);
        assert_eq!(line["os"], "web");
        assert_eq!(line["account"], "u1");
        assert_eq!(line["data"]["message"], "to <email>");
        let report = e.report(&s, "t", "help a@b.co", vec![("page.log".into(), "one a@b.co\ntwo".into())], json!([]), None);
        assert_eq!(report["note"], "help <email>");
        assert_eq!(report["logs"][0]["text"], "one <email>\ntwo");
        assert!(report.get("account").is_none());
    }
}

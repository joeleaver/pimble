//! Every line of `fixtures/log_lines.corpus` scrubbed (docs/DIAGNOSTICS_CONTRACT.md
//! section 2): nothing private may survive, and what helps (ids, store names,
//! paths below the home folder) must.

use pimble_core::diagnostics::Scrubber;

const PRIVATE: &[&str] = &[
    "/home/tester",
    "tester.one@example.com",
    "friend+notes@example.org",
    "Secret Recipe",
    "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9",
    "3f9a8b7c6d5e4f30a1b2c3d4e5f60718293a4b5c",
    "9f8e7d6c5b4a39281706f5e4d3c2b1a0ffeeddcc",
];

#[test]
fn nothing_private_survives_and_what_helps_does() {
    let mut scrubber = Scrubber::new(Some("/home/tester"));
    scrubber.forget_title("Secret Recipe");
    let lines: Vec<&str> = include_str!("fixtures/log_lines.corpus").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).collect();
    let scrubbed: Vec<String> = lines.iter().map(|l| scrubber.scrub(l)).collect();
    for (line, out) in lines.iter().zip(&scrubbed) {
        for private in PRIVATE {
            assert!(!out.contains(private), "{private:?} survived in\n  {line}\n  -> {out}");
        }
    }
    let all = scrubbed.join("\n");
    for kept in ["'Garden Club'", "~/Dropbox/___Household/Garden Club.pimble", "280e81c4-8e99-4f62-a045-c14f5d056a9b", "pid 3819851", "https://pimble.app/api/v1/token answered 502", "~/.cache/huggingface/token"] {
        assert!(all.contains(kept), "{kept:?} should be kept:\n{all}");
    }
}

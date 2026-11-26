/// Prints a notice sealed here, for web/build/notice.test.mjs (the
/// service worker's opening of notices, in JavaScript):
/// `cargo test -p pimble-crypto print_a_notice_vector -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_a_notice_vector() {
    let key = super::NoticeKey { secret: [7u8; 32] };
    let sealed = super::seal_notice(br#"{"id":"n1","snippet":"@Will can you check the oven"}"#, &key.public(), "will-account").unwrap();
    println!("secret_hex={}", hex_of(&key.secret));
    println!("public={}", key.public());
    println!("sealed={sealed}");
    assert!(super::open_notice(&sealed, &key, "will-account").is_ok());
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

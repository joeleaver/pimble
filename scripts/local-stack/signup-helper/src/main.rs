//! Throwaway: sign an address up on a local accounts service with real key
//! material, the way the web app's signup page does. `signup-helper <url> <email> <password>`.
use pimble_crypto::*;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (url, email, password) = (&args[1], &args[2], &args[3]);
    let kdf = KdfParams::generate();
    let pk = derive_password_keys(password, &kdf).unwrap();
    let keys = AccountKeys::generate();
    let rkdf = KdfParams::generate();
    let rkek = derive_recovery_kek(&generate_recovery_code(), &rkdf).unwrap();
    let body = serde_json::json!({
        "email": email,
        "auth_key": encode_auth_key(&pk.auth_key),
        "kdf": kdf,
        "public_keys": keys.public_keys(),
        "account_key_blob": wrap_account_keys(&keys, &pk.kek).unwrap(),
        "recovery_salt": rkdf.salt,
        "recovery_key_blob": wrap_account_keys(&keys, &rkek).unwrap(),
    });
    let resp = reqwest::blocking::Client::new().post(format!("{url}/api/v1/signup")).json(&body).send().unwrap();
    println!("signup {} -> {}", email, resp.status());
}

#[test]
fn the_personal_key_is_the_accounts_and_seals_to_it() {
    let keys = super::AccountKeys::generate();
    let again = super::AccountKeys { encryption_secret: keys.encryption_secret, signing_secret: keys.signing_secret };
    assert_eq!(super::personal_key(&keys).0, super::personal_key(&again).0);
    let other = super::AccountKeys::generate();
    assert_ne!(super::personal_key(&keys).0, super::personal_key(&other).0);
    let aad = super::personal_aad("sub-1");
    let blob = super::Blob::encrypt(&super::personal_key(&keys), uuid::Uuid::nil(), &aad, b"read state");
    assert_eq!(super::Blob::decrypt(&super::personal_key(&keys), &aad, &blob).unwrap(), b"read state");
    assert!(super::Blob::decrypt(&super::personal_key(&keys), &super::personal_aad("sub-2"), &blob).is_err());
}

#[test]
fn the_chosen_watches_do_not_open_as_the_personal_document() {
    let keys = super::AccountKeys::generate();
    let key = super::personal_key(&keys);
    let blob = super::Blob::encrypt(&key, uuid::Uuid::nil(), &super::watches_aad("sub-1"), b"[]");
    assert_eq!(super::Blob::decrypt(&key, &super::watches_aad("sub-1"), &blob).unwrap(), b"[]");
    assert!(super::Blob::decrypt(&key, &super::personal_aad("sub-1"), &blob).is_err());
    assert!(super::Blob::decrypt(&key, &super::watches_aad("sub-2"), &blob).is_err());
}

use super::*;

#[test]
fn a_data_key_wraps_under_a_scope_key_and_only_unwraps_for_its_document() {
    let dek = SymmetricKey::generate();
    let store_key = SymmetricKey::generate();
    let share_key = SymmetricKey::generate();
    let aad = dek_aad("store", "doc");

    let under_store = wrap_dek(&dek, &store_key, KeyId::new_v4(), &aad);
    let under_share = wrap_dek(&dek, &share_key, KeyId::new_v4(), &aad);
    assert_eq!(unwrap_dek(&under_store, &store_key, &aad).unwrap().0, dek.0);
    assert_eq!(unwrap_dek(&under_share, &share_key, &aad).unwrap().0, dek.0);

    // The wrong scope key, another document, and a tampered wrap all fail.
    assert!(matches!(unwrap_dek(&under_store, &share_key, &aad), Err(CryptoError::Decrypt)));
    assert!(matches!(unwrap_dek(&under_store, &store_key, &dek_aad("store", "other")), Err(CryptoError::Decrypt)));
    let mut tampered = under_store.clone();
    tampered.ciphertext = b64_encode(&[0u8; 48]);
    assert!(matches!(unwrap_dek(&tampered, &store_key, &aad), Err(CryptoError::Decrypt)));

    // JSON round trip, as the server stores and relays it.
    let json = serde_json::to_string(&under_share).unwrap();
    let back: WrappedDek = serde_json::from_str(&json).unwrap();
    assert_eq!(back, under_share);
}

// ---- Known-answer tests -------------------------------------------------------

/// RFC 5869 Appendix A.1, Test Case 1 (HKDF-SHA256, basic test case).
#[test]
fn hkdf_sha256_rfc5869_test_case_1() {
    let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
    let salt = hex::decode("000102030405060708090a0b0c").unwrap();
    let info = hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap();
    let expected_prk =
        hex::decode("077709362c2e32df0ddc3f0dc47bba63 90b6c73bb50f9c3122ec844ad7c2b3e5".replace(' ', ""))
            .unwrap();
    let expected_okm = hex::decode(
        "3cb25f25faacd57a90434f64d0362f2a 2d2d0a90cf1a5a4c5db02d56ecc4c5bf 34007208d5b887185865"
            .replace(' ', ""),
    )
    .unwrap();

    let (prk, hk) = Hkdf::<Sha256>::extract(Some(&salt), &ikm);
    assert_eq!(prk.as_slice(), expected_prk.as_slice());

    let mut okm = [0u8; 42];
    hk.expand(&info, &mut okm).unwrap();
    assert_eq!(okm.as_slice(), expected_okm.as_slice());
}

/// draft-irtf-cfrg-xchacha-03 Appendix A.3.1, the developer-friendly
/// AEAD_XCHACHA20_POLY1305 test vector.
#[test]
fn xchacha20poly1305_draft_vector() {
    let key = hex::decode("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f").unwrap();
    let nonce = hex::decode("404142434445464748494a4b4c4d4e4f5051525354555657").unwrap();
    let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
    let plaintext = hex::decode(
        "4c616469657320616e642047656e746c656d656e206f662074686520636c6173\
         73206f66202739393a204966204920636f756c64206f6666657220796f75206f\
         6e6c79206f6e652074697020666f7220746865206675747572652c2073756e73\
         637265656e20776f756c642062652069742e",
    )
    .unwrap();
    let ciphertext = hex::decode(
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb\
         731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452\
         2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9\
         21f9664c97637da9768812f615c68b13b52e",
    )
    .unwrap();
    let tag = hex::decode("c0875924c1c7987947deafd8780acf49").unwrap();

    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key));
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .unwrap();

    let mut expected = ciphertext.clone();
    expected.extend_from_slice(&tag);
    assert_eq!(sealed, expected);

    let opened = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &sealed,
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(opened, plaintext);
}

/// Argon2id at the contract's parameters (32 MiB, t=3, p=1) against a fixed
/// input/salt. Not from an external KAT (RFC 9106's vectors use different
/// parameters); computed once with this implementation and pinned here as a
/// regression so a future change to the KDF wiring is caught.
#[test]
fn argon2id_fixed_input_regression() {
    let params = KdfParams {
        salt: b64_encode(b"0123456789abcdef"), // 16 bytes
        m_cost: KDF_M_COST_KIB,
        t_cost: KDF_T_COST,
        p_cost: KDF_P_COST,
    };
    let out = argon2id_hash(b"correct horse battery staple", &params, 64).unwrap();
    assert_eq!(out.len(), 64);
    let expected = hex::decode(
        "282e6b05d59dbc214153c48c2198f2bfceaa2ec7a1b513fd8b1b97402ac0ec3\
         8d40d37b5270ca6ebd113eda3bb09d0eef1c2c384e3c9285e24f783c3b6fc9f81",
    )
    .unwrap();
    // Self-computed regression vector (see doc comment): this assertion pins the
    // value this test observed the first time it ran, recorded so a later change
    // to the KDF wiring is caught. If argon2id_hash is intentionally changed,
    // regenerate this constant from the new implementation's output.
    assert_eq!(out, expected, "argon2id regression vector changed: {}", hex::encode(&out));
}

// ---- Round trips ----------------------------------------------------------------

#[test]
fn symmetric_key_generate_is_random() {
    let a = SymmetricKey::generate();
    let b = SymmetricKey::generate();
    assert_ne!(a.0, b.0);
}

#[test]
fn password_keys_round_trip_and_deterministic() {
    let params = KdfParams::generate();
    let a = derive_password_keys("hunter2", &params).unwrap();
    let b = derive_password_keys("hunter2", &params).unwrap();
    assert_eq!(a.auth_key, b.auth_key);
    assert_eq!(a.kek.0, b.kek.0);
    assert_ne!(a.auth_key, a.kek.0);

    let different = derive_password_keys("hunter3", &params).unwrap();
    assert_ne!(a.auth_key, different.auth_key);
}

#[test]
fn encode_auth_key_is_base64url() {
    let key = [7u8; 32];
    let encoded = encode_auth_key(&key);
    let decoded = b64_decode(&encoded).unwrap();
    assert_eq!(decoded, key);
    assert!(!encoded.contains('+'));
    assert!(!encoded.contains('/'));
    assert!(!encoded.contains('='));
}

#[test]
fn account_keys_wrap_unwrap_round_trip() {
    let keys = AccountKeys::generate();
    let kek = SymmetricKey::generate();
    let blob = wrap_account_keys(&keys, &kek).unwrap();
    let recovered = unwrap_account_keys(&blob, &kek).unwrap();
    assert_eq!(recovered.encryption_secret, keys.encryption_secret);
    assert_eq!(recovered.signing_secret, keys.signing_secret);
}

#[test]
fn account_key_blob_wrong_key_fails() {
    let keys = AccountKeys::generate();
    let kek = SymmetricKey::generate();
    let wrong_kek = SymmetricKey::generate();
    let blob = wrap_account_keys(&keys, &kek).unwrap();
    let result = unwrap_account_keys(&blob, &wrong_kek);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn account_key_blob_tamper_detected() {
    let keys = AccountKeys::generate();
    let kek = SymmetricKey::generate();
    let mut blob = wrap_account_keys(&keys, &kek).unwrap();
    let mut raw = b64_decode(&blob.ciphertext).unwrap();
    raw[0] ^= 0xff;
    blob.ciphertext = b64_encode(&raw);
    let result = unwrap_account_keys(&blob, &kek);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn account_key_blob_json_round_trip_field_names() {
    let keys = AccountKeys::generate();
    let kek = SymmetricKey::generate();
    let blob = wrap_account_keys(&keys, &kek).unwrap();
    let json = serde_json::to_value(&blob).unwrap();
    assert_eq!(json.get("v").unwrap().as_u64().unwrap(), 1);
    assert!(json.get("nonce").unwrap().is_string());
    assert!(json.get("ciphertext").unwrap().is_string());
    let round_tripped: AccountKeyBlob = serde_json::from_value(json).unwrap();
    assert_eq!(round_tripped, blob);
}

#[test]
fn recovery_code_derives_matching_kek() {
    let code = generate_recovery_code();
    // 8 groups of 4 chars joined by '-' = 32 chars + 7 dashes.
    assert_eq!(code.len(), 32 + 7);
    assert_eq!(code.matches('-').count(), 7);

    let params = KdfParams::generate();
    let a = derive_recovery_kek(&code, &params).unwrap();
    let b = derive_recovery_kek(&code, &params).unwrap();
    assert_eq!(a.0, b.0);
}

#[test]
fn recovery_code_normalization_accepts_variants() {
    let code = generate_recovery_code();
    let params = KdfParams::generate();
    let canonical = derive_recovery_kek(&code, &params).unwrap();

    let lower = code.to_lowercase();
    let no_dashes = code.replace('-', "");
    let extra_spaces = code.replace('-', "  -  ").to_lowercase();
    let mixed = format!(" {} ", code.replace('-', " "));

    for variant in [lower, no_dashes, extra_spaces, mixed] {
        let derived = derive_recovery_kek(&variant, &params).unwrap();
        assert_eq!(derived.0, canonical.0, "variant {variant:?} did not normalize");
    }
}

#[test]
fn recovery_kek_different_from_password_kek() {
    let params = KdfParams::generate();
    let code = generate_recovery_code();
    let recovery_kek = derive_recovery_kek(&code, &params).unwrap();
    let password_keys = derive_password_keys("hunter2", &params).unwrap();
    assert_ne!(recovery_kek.0, password_keys.kek.0);
}

#[test]
fn key_envelope_wrap_unwrap_round_trip() {
    let sender = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let recipient_public = recipient.public_keys();
    let sender_public = sender.public_keys();

    let key = SymmetricKey::generate();
    let key_id = KeyId::new_v4();
    let envelope = wrap_key(&key, key_id, &recipient_public, &sender, "store:abc").unwrap();
    assert_eq!(envelope.key_id, key_id);
    assert_eq!(envelope.context, "store:abc");

    let recovered = unwrap_key(&envelope, &recipient, &sender_public.signing).unwrap();
    assert_eq!(recovered.0, key.0);
}

#[test]
fn key_envelope_wrong_recipient_fails() {
    let sender = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let someone_else = AccountKeys::generate();
    let recipient_public = recipient.public_keys();
    let sender_public = sender.public_keys();

    let key = SymmetricKey::generate();
    let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

    let result = unwrap_key(&envelope, &someone_else, &sender_public.signing);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn key_envelope_wrong_expected_signer_fails() {
    let sender = AccountKeys::generate();
    let impostor = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let recipient_public = recipient.public_keys();
    let impostor_public = impostor.public_keys();

    let key = SymmetricKey::generate();
    let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

    // Verifying against a different signer's public key than the one that
    // actually signed must fail signature verification.
    let result = unwrap_key(&envelope, &recipient, &impostor_public.signing);
    assert!(matches!(result, Err(CryptoError::BadSignature)));
}

#[test]
fn key_envelope_tamper_detected() {
    let sender = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let recipient_public = recipient.public_keys();
    let sender_public = sender.public_keys();

    let key = SymmetricKey::generate();
    let mut envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();
    let mut raw = b64_decode(&envelope.ciphertext).unwrap();
    raw[0] ^= 0xff;
    envelope.ciphertext = b64_encode(&raw);

    let result = unwrap_key(&envelope, &recipient, &sender_public.signing);
    assert!(matches!(result, Err(CryptoError::BadSignature)));
}

#[test]
fn verify_envelope_accepts_correct_and_rejects_tampered() {
    let sender = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let recipient_public = recipient.public_keys();
    let sender_public = sender.public_keys();

    let key = SymmetricKey::generate();
    let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

    // A correct envelope verifies with no private key at all.
    assert!(verify_envelope(&envelope, &sender_public.signing).is_ok());

    // Tampering with any signed field is caught.
    let mut tampered_context = envelope.clone();
    tampered_context.context = "store:other".to_string();
    assert!(matches!(
        verify_envelope(&tampered_context, &sender_public.signing),
        Err(CryptoError::BadSignature)
    ));

    let mut tampered_key_id = envelope.clone();
    tampered_key_id.key_id = KeyId::new_v4();
    assert!(matches!(
        verify_envelope(&tampered_key_id, &sender_public.signing),
        Err(CryptoError::BadSignature)
    ));

    let mut tampered_ciphertext = envelope.clone();
    let mut raw = b64_decode(&tampered_ciphertext.ciphertext).unwrap();
    raw[0] ^= 0xff;
    tampered_ciphertext.ciphertext = b64_encode(&raw);
    assert!(matches!(
        verify_envelope(&tampered_ciphertext, &sender_public.signing),
        Err(CryptoError::BadSignature)
    ));

    // A claimed signer that doesn't match the envelope's own `signer` field is
    // rejected before any signature math.
    let impostor = AccountKeys::generate().public_keys();
    assert!(matches!(
        verify_envelope(&envelope, &impostor.signing),
        Err(CryptoError::BadSignature)
    ));
}

#[test]
fn key_envelope_json_round_trip_field_names() {
    let sender = AccountKeys::generate();
    let recipient = AccountKeys::generate();
    let recipient_public = recipient.public_keys();

    let key = SymmetricKey::generate();
    let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "share:xyz").unwrap();
    let json = serde_json::to_value(&envelope).unwrap();
    for field in [
        "v",
        "key_id",
        "recipient",
        "ephemeral",
        "nonce",
        "ciphertext",
        "signer",
        "signature",
        "context",
    ] {
        assert!(json.get(field).is_some(), "missing field {field}");
    }
    let round_tripped: KeyEnvelope = serde_json::from_value(json).unwrap();
    assert_eq!(round_tripped, envelope);
}

#[test]
fn blob_round_trip() {
    let key = SymmetricKey::generate();
    let key_id = KeyId::new_v4();
    let aad = blob_aad("store-1", "node-1");
    let plaintext = b"hello, encrypted world";

    let blob = Blob::encrypt(&key, key_id, &aad, plaintext);
    assert_eq!(&blob[0..2], &BLOB_MAGIC);
    assert_eq!(blob[2], VERSION);

    assert_eq!(Blob::key_id(&blob).unwrap(), key_id);

    let decrypted = Blob::decrypt(&key, &aad, &blob).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn blob_key_id_readable_without_key() {
    let key = SymmetricKey::generate();
    let key_id = KeyId::new_v4();
    let aad = blob_aad("s", "d");
    let blob = Blob::encrypt(&key, key_id, &aad, b"data");
    assert_eq!(Blob::key_id(&blob).unwrap(), key_id);
}

#[test]
fn blob_wrong_key_fails() {
    let key = SymmetricKey::generate();
    let wrong_key = SymmetricKey::generate();
    let aad = blob_aad("s", "d");
    let blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
    let result = Blob::decrypt(&wrong_key, &aad, &blob);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn blob_wrong_aad_fails() {
    let key = SymmetricKey::generate();
    let blob = Blob::encrypt(&key, KeyId::new_v4(), &blob_aad("s", "d"), b"data");
    let result = Blob::decrypt(&key, &blob_aad("s", "other-doc"), &blob);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn blob_tamper_detected() {
    let key = SymmetricKey::generate();
    let aad = blob_aad("s", "d");
    let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
    let last = blob.len() - 1;
    blob[last] ^= 0xff;
    let result = Blob::decrypt(&key, &aad, &blob);
    assert!(matches!(result, Err(CryptoError::Decrypt)));
}

#[test]
fn blob_wrong_magic_rejected() {
    let key = SymmetricKey::generate();
    let aad = blob_aad("s", "d");
    let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
    blob[0] = b'X';
    assert!(matches!(Blob::key_id(&blob), Err(CryptoError::Malformed(_))));
    assert!(matches!(Blob::decrypt(&key, &aad, &blob), Err(CryptoError::Malformed(_))));
}

#[test]
fn blob_unsupported_version_rejected() {
    let key = SymmetricKey::generate();
    let aad = blob_aad("s", "d");
    let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
    blob[2] = 99;
    assert!(matches!(Blob::key_id(&blob), Err(CryptoError::UnsupportedVersion(99))));
}

#[test]
fn blob_too_short_rejected() {
    let short = vec![b'P', b'B', 1];
    assert!(matches!(Blob::key_id(&short), Err(CryptoError::Malformed(_))));
}

#[test]
fn kdf_params_json_round_trip() {
    let params = KdfParams::generate();
    let json = serde_json::to_string(&params).unwrap();
    let round_tripped: KdfParams = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, params);
}

#[test]
fn account_public_keys_json_round_trip() {
    let keys = AccountKeys::generate();
    let public = keys.public_keys();
    let json = serde_json::to_string(&public).unwrap();
    let round_tripped: AccountPublicKeys = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, public);
}

#[test]
fn a_notice_opens_with_its_recipients_notice_key_only() {
    let will = NoticeKey::generate();
    let sealed = seal_notice(b"{\"snippet\":\"hi\"}", &will.public(), "will").unwrap();
    assert_eq!(open_notice(&sealed, &will, "will").unwrap(), b"{\"snippet\":\"hi\"}");
    assert!(matches!(open_notice(&sealed, &will, "sam"), Err(CryptoError::Decrypt)), "sealed to Will, never opens as Sam's");
    assert!(matches!(open_notice(&sealed, &NoticeKey::generate(), "will"), Err(CryptoError::Decrypt)));
    let mut tampered = URL_SAFE_NO_PAD.decode(&sealed).unwrap();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(open_notice(&URL_SAFE_NO_PAD.encode(&tampered), &will, "will").is_err());
    assert!(matches!(open_notice("AAAA", &will, "will"), Err(CryptoError::Malformed(_))));
}

#[test]
fn the_notice_key_is_wrapped_to_the_account_itself() {
    let me = AccountKeys::generate();
    let notice = NoticeKey::generate();
    let envelope = wrap_notice_key(&notice, &me, "u1").unwrap();
    let back = unwrap_notice_key(&envelope, &me, "u1", &notice.public()).unwrap();
    assert_eq!(back.secret, notice.secret);
    // Another account's keys, another user id, another public half: refused.
    assert!(unwrap_notice_key(&envelope, &AccountKeys::generate(), "u1", &notice.public()).is_err());
    assert!(unwrap_notice_key(&envelope, &me, "u2", &notice.public()).is_err());
    assert!(unwrap_notice_key(&envelope, &me, "u1", &NoticeKey::generate().public()).is_err());
    // A store key's envelope never opens as a notice key.
    let store = wrap_key(&SymmetricKey(notice.secret), KeyId::nil(), &me.public_keys(), &me, "store:s").unwrap();
    assert!(unwrap_notice_key(&store, &me, "u1", &notice.public()).is_err());
}

// Opening a sealed notice in the service worker (docs/NOTIFICATIONS_CONTRACT.md,
// wave 2), with no page open and no account unlocked: the same construction
// as pimble_crypto::open_notice, in plain JavaScript over WebCrypto.
//
//   sealed = version (1) · ephemeral X25519 public (32) · nonce (24) · ciphertext
//   key    = HKDF-SHA256(salt = ephemeral ‖ recipient public, ikm = X25519 shared,
//                        info = "pimble-notice"), 32 bytes
//   plain  = XChaCha20-Poly1305 open(key, nonce, ciphertext,
//                                    aad = "pimble-notice/<recipient account>")
//
// The X25519 secret is the notice key the browser's own server keeps in
// IndexedDB (`pimble-device`, `keys`, `notice`) as a non-extractable CryptoKey:
// WebCrypto derives with it and never gives it to script. WebCrypto has no
// XChaCha20-Poly1305, so that part is written out here (RFC 8439 and the
// XChaCha draft); it runs over a few hundred bytes.
//
// Concatenated in front of sw.js by build/post_build.sh; tested by
// build/notice.test.mjs against a notice pimble_crypto sealed.
(function (global) {
    "use strict";

    function b64urlDecode(text) {
        const b64 = text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4);
        const raw = atob(b64);
        const out = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
        return out;
    }

    function concat(...parts) {
        const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
        let at = 0;
        for (const p of parts) {
            out.set(p, at);
            at += p.length;
        }
        return out;
    }

    // ── ChaCha20 (RFC 8439 section 2.3) ──────────────────────────────────

    function rotl(v, n) {
        return (v << n) | (v >>> (32 - n));
    }

    function quarter(s, a, b, c, d) {
        s[a] = (s[a] + s[b]) | 0; s[d] = rotl(s[d] ^ s[a], 16);
        s[c] = (s[c] + s[d]) | 0; s[b] = rotl(s[b] ^ s[c], 12);
        s[a] = (s[a] + s[b]) | 0; s[d] = rotl(s[d] ^ s[a], 8);
        s[c] = (s[c] + s[d]) | 0; s[b] = rotl(s[b] ^ s[c], 7);
    }

    function words(bytes) {
        const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        const out = new Uint32Array(bytes.length / 4);
        for (let i = 0; i < out.length; i++) out[i] = dv.getUint32(i * 4, true);
        return out;
    }

    function rounds(state) {
        const s = Uint32Array.from(state);
        for (let i = 0; i < 10; i++) {
            quarter(s, 0, 4, 8, 12); quarter(s, 1, 5, 9, 13); quarter(s, 2, 6, 10, 14); quarter(s, 3, 7, 11, 15);
            quarter(s, 0, 5, 10, 15); quarter(s, 1, 6, 11, 12); quarter(s, 2, 7, 8, 13); quarter(s, 3, 4, 9, 14);
        }
        return s;
    }

    const SIGMA = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574];

    function initial(key, counter, nonce12) {
        const k = words(key);
        const n = words(nonce12);
        return Uint32Array.of(...SIGMA, ...k, counter, ...n);
    }

    function block(key, counter, nonce12) {
        const state = initial(key, counter, nonce12);
        const s = rounds(state);
        const out = new Uint8Array(64);
        const dv = new DataView(out.buffer);
        for (let i = 0; i < 16; i++) dv.setUint32(i * 4, (s[i] + state[i]) >>> 0, true);
        return out;
    }

    function chacha20(key, counter, nonce12, data) {
        const out = new Uint8Array(data.length);
        for (let at = 0, c = counter; at < data.length; at += 64, c++) {
            const ks = block(key, c, nonce12);
            for (let i = 0; i < 64 && at + i < data.length; i++) out[at + i] = data[at + i] ^ ks[i];
        }
        return out;
    }

    // HChaCha20 (draft-irtf-cfrg-xchacha section 2.2): the subkey from the
    // key and the nonce's first 16 bytes.
    function hchacha20(key, nonce16) {
        const k = words(key);
        const n = words(nonce16);
        const s = rounds(Uint32Array.of(...SIGMA, ...k, ...n));
        const out = new Uint8Array(32);
        const dv = new DataView(out.buffer);
        [0, 1, 2, 3, 12, 13, 14, 15].forEach((w, i) => dv.setUint32(i * 4, s[w], true));
        return out;
    }

    // ── Poly1305 (RFC 8439 section 2.5), over BigInt ──────────────────────

    function leBig(bytes) {
        let v = 0n;
        for (let i = bytes.length - 1; i >= 0; i--) v = (v << 8n) | BigInt(bytes[i]);
        return v;
    }

    function poly1305(key, msg) {
        const P = (1n << 130n) - 5n;
        const r = leBig(key.subarray(0, 16)) & 0x0ffffffc0ffffffc0ffffffc0fffffffn;
        const s = leBig(key.subarray(16, 32));
        let acc = 0n;
        for (let at = 0; at < msg.length; at += 16) {
            const chunk = msg.subarray(at, Math.min(at + 16, msg.length));
            const n = leBig(chunk) | (1n << BigInt(8 * chunk.length));
            acc = ((acc + n) * r) % P;
        }
        acc = (acc + s) & ((1n << 128n) - 1n);
        const tag = new Uint8Array(16);
        for (let i = 0; i < 16; i++) {
            tag[i] = Number(acc & 0xffn);
            acc >>= 8n;
        }
        return tag;
    }

    function pad16(n) {
        return new Uint8Array((16 - (n % 16)) % 16);
    }

    function le64(n) {
        const out = new Uint8Array(8);
        new DataView(out.buffer).setBigUint64(0, BigInt(n), true);
        return out;
    }

    // XChaCha20-Poly1305 open; throws when the tag does not match.
    function xchachaOpen(key, nonce24, sealed, aad) {
        if (sealed.length < 16) throw new Error("a sealed notice too short");
        const subkey = hchacha20(key, nonce24.subarray(0, 16));
        const nonce12 = concat(new Uint8Array(4), nonce24.subarray(16, 24));
        const ct = sealed.subarray(0, sealed.length - 16);
        const tag = sealed.subarray(sealed.length - 16);
        const polyKey = block(subkey, 0, nonce12).subarray(0, 32);
        const expected = poly1305(polyKey, concat(aad, pad16(aad.length), ct, pad16(ct.length), le64(aad.length), le64(ct.length)));
        let diff = 0;
        for (let i = 0; i < 16; i++) diff |= expected[i] ^ tag[i];
        if (diff !== 0) throw new Error("the notice does not open with this key");
        return chacha20(subkey, 1, nonce12, ct);
    }

    // ── The notice ────────────────────────────────────────────────────────

    // `record` is what the browser's own server kept: { sub, key, public }.
    // Answers the notice's plaintext bytes; throws when it does not open.
    async function open(sealedText, record, recipient) {
        const subtle = global.crypto.subtle;
        const bytes = b64urlDecode(sealedText);
        if (bytes.length < 1 + 32 + 24 + 16 || bytes[0] !== 1) throw new Error("not a sealed notice");
        const ephemeral = bytes.subarray(1, 33);
        const nonce = bytes.subarray(33, 57);
        const body = bytes.subarray(57);
        const theirs = await subtle.importKey("raw", ephemeral, { name: "X25519" }, true, []);
        const shared = new Uint8Array(await subtle.deriveBits({ name: "X25519", public: theirs }, record.key, 256));
        const ikm = await subtle.importKey("raw", shared, "HKDF", false, ["deriveBits"]);
        const enc = new TextEncoder();
        const salt = concat(ephemeral, b64urlDecode(record.public));
        const key = new Uint8Array(await subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info: enc.encode("pimble-notice") }, ikm, 256));
        return xchachaOpen(key, nonce, body, enc.encode("pimble-notice/" + recipient));
    }

    global.pimbleNotice = { open, xchachaOpen, poly1305, hchacha20, b64urlDecode };
})(typeof self !== "undefined" ? self : globalThis);

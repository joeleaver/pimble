//! The remembered device (docs/ONE_CODEBASE_CONTRACT.md section 7): a
//! non-extractable AES-GCM key made with WebCrypto on the first remembered
//! sign-in in this browser and kept, as a `CryptoKey`, in IndexedDB. It never
//! leaves WebCrypto. It wraps the two things kept beside it:
//!
//! - the **disk key**, 32 random bytes that seal every file the server keeps in
//!   OPFS ([`pimble_host::SealedDisk`]);
//! - the **keystore**, the signed-in account's unwrapped keys and the store
//!   keys the server holds ([`DeviceKeyRest`], the worker's `KeyRest`).
//!
//! Forgetting deletes the device key first, so what OPFS and IndexedDB still
//! hold is unreadable even if clearing them fails or is interrupted.
//!
//! One database, `pimble-device`, one object store, `keys`: `device` (the
//! `CryptoKey`), `disk` and `keystore` (each `{ iv, data }`, AES-GCM with a
//! fresh 12-byte iv). Opened per operation and closed after, so deleting the
//! database is never held up by a connection of this worker's.

use std::io;

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{CryptoKey, IdbDatabase, IdbFactory, IdbOpenDbRequest, IdbRequest, IdbTransactionMode, SubtleCrypto};

const DB: &str = "pimble-device";
const STORE: &str = "keys";
const DEVICE: &str = "device";
const DISK: &str = "disk";
const KEYSTORE: &str = "keystore";

fn failed(what: &str, e: JsValue) -> io::Error {
    let message = Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()).or_else(|| e.as_string()).unwrap_or_else(|| format!("{e:?}"));
    io::Error::other(format!("{what}: {message}"))
}

fn global(name: &str) -> io::Result<JsValue> {
    let value = Reflect::get(&js_sys::global(), &JsValue::from_str(name)).map_err(|e| failed(name, e))?;
    if value.is_undefined() || value.is_null() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, format!("this context has no {name}")));
    }
    Ok(value)
}

fn subtle() -> io::Result<SubtleCrypto> {
    let crypto = global("crypto")?;
    Ok(Reflect::get(&crypto, &JsValue::from_str("subtle")).map_err(|e| failed("crypto.subtle", e))?.unchecked_into())
}

/// A request's answer: its `result` once `success` fires.
async fn answered(request: &IdbRequest) -> io::Result<JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    });
    JsFuture::from(promise).await.map_err(|e| failed("IndexedDB", request.error().ok().flatten().map(JsValue::from).unwrap_or(e)))?;
    request.result().map_err(|e| failed("IndexedDB result", e))
}

/// The database, made when `create`; `None` when it is not there and not to
/// be made (so asking whether this browser remembers itself never leaves an
/// empty database behind, as after "Forget This Browser").
async fn open(create: bool) -> io::Result<Option<IdbDatabase>> {
    let factory: IdbFactory = global("indexedDB")?.unchecked_into();
    let request: IdbOpenDbRequest = factory.open_with_u32(DB, 1).map_err(|e| failed("opening the device database", e))?;
    let upgrade = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let Some(target) = event.target() else { return };
        let request: IdbOpenDbRequest = target.unchecked_into();
        if !create {
            // Not there: undo its making.
            if let Some(tx) = request.transaction() {
                let _ = tx.abort();
            }
            return;
        }
        if let Ok(db) = request.result() {
            let db: IdbDatabase = db.unchecked_into();
            if !db.object_store_names().contains(STORE) {
                let _ = db.create_object_store(STORE);
            }
        }
    });
    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let db = answered(&request).await;
    request.set_onupgradeneeded(None);
    drop(upgrade);
    match db {
        Ok(db) => Ok(Some(db.unchecked_into())),
        Err(_) if !create => Ok(None),
        Err(e) => Err(e),
    }
}

/// The transaction's end, which is when a write is on the disk.
async fn completed(tx: &web_sys::IdbTransaction) -> io::Result<()> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        tx.set_oncomplete(Some(&resolve));
        tx.set_onerror(Some(&reject));
        tx.set_onabort(Some(&reject));
    });
    JsFuture::from(promise).await.map(|_| ()).map_err(|e| failed("IndexedDB transaction", e))
}

async fn get(name: &str) -> io::Result<Option<JsValue>> {
    let Some(db) = open(false).await? else { return Ok(None) };
    let got = async {
        let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readonly).map_err(|e| failed("IndexedDB", e))?;
        let store = tx.object_store(STORE).map_err(|e| failed("IndexedDB", e))?;
        let value = answered(&store.get(&JsValue::from_str(name)).map_err(|e| failed("IndexedDB get", e))?).await?;
        Ok::<_, io::Error>((!value.is_undefined() && !value.is_null()).then_some(value))
    }
    .await;
    db.close();
    got
}

async fn put(name: &str, value: &JsValue) -> io::Result<()> {
    let db = open(true).await?.ok_or_else(|| io::Error::other("the device database was not made"))?;
    let put = async {
        let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(|e| failed("IndexedDB", e))?;
        let store = tx.object_store(STORE).map_err(|e| failed("IndexedDB", e))?;
        store.put_with_key(value, &JsValue::from_str(name)).map_err(|e| failed("IndexedDB put", e))?;
        completed(&tx).await
    }
    .await;
    db.close();
    put
}

async fn delete(name: &str) -> io::Result<()> {
    let Some(db) = open(false).await? else { return Ok(()) };
    let deleted = async {
        let tx = db.transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite).map_err(|e| failed("IndexedDB", e))?;
        let store = tx.object_store(STORE).map_err(|e| failed("IndexedDB", e))?;
        store.delete(&JsValue::from_str(name)).map_err(|e| failed("IndexedDB delete", e))?;
        completed(&tx).await
    }
    .await;
    db.close();
    deleted
}

fn aes_gcm(iv: Option<&[u8]>) -> Object {
    let params = Object::new();
    let _ = Reflect::set(&params, &JsValue::from_str("name"), &JsValue::from_str("AES-GCM"));
    if let Some(iv) = iv {
        let _ = Reflect::set(&params, &JsValue::from_str("iv"), &Uint8Array::from(iv));
    }
    params
}

/// The device key, made (and kept) when `create` and there is none.
async fn device_key(create: bool) -> io::Result<Option<CryptoKey>> {
    if let Some(key) = get(DEVICE).await? {
        return Ok(Some(key.unchecked_into()));
    }
    if !create {
        return Ok(None);
    }
    let params = aes_gcm(None);
    let _ = Reflect::set(&params, &JsValue::from_str("length"), &JsValue::from_f64(256.0));
    let usages = Array::of2(&JsValue::from_str("encrypt"), &JsValue::from_str("decrypt"));
    let key = JsFuture::from(subtle()?.generate_key_with_object(&params, false, &usages).map_err(|e| failed("making the device key", e))?)
        .await
        .map_err(|e| failed("making the device key", e))?;
    put(DEVICE, &key).await?;
    tracing::info!("This browser now remembers itself: a device key was made");
    Ok(Some(key.unchecked_into()))
}

async fn seal(key: &CryptoKey, plain: &[u8]) -> io::Result<JsValue> {
    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut iv).map_err(|e| io::Error::other(e.to_string()))?;
    let data = JsFuture::from(subtle()?.encrypt_with_object_and_u8_array(&aes_gcm(Some(&iv)), key, plain).map_err(|e| failed("sealing", e))?)
        .await
        .map_err(|e| failed("sealing", e))?;
    let record = Object::new();
    let _ = Reflect::set(&record, &JsValue::from_str("iv"), &Uint8Array::from(&iv[..]));
    let _ = Reflect::set(&record, &JsValue::from_str("data"), &Uint8Array::new(&data));
    Ok(record.into())
}

async fn open_sealed(key: &CryptoKey, record: &JsValue) -> io::Result<Vec<u8>> {
    let iv = Uint8Array::new(&Reflect::get(record, &JsValue::from_str("iv")).map_err(|e| failed("a sealed record", e))?).to_vec();
    let data = Uint8Array::new(&Reflect::get(record, &JsValue::from_str("data")).map_err(|e| failed("a sealed record", e))?).to_vec();
    let plain = JsFuture::from(subtle()?.decrypt_with_object_and_u8_array(&aes_gcm(Some(&iv)), key, &data).map_err(|e| failed("opening", e))?)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("does not open with this browser's device key: {}", failed("", e))))?;
    Ok(Uint8Array::new(&plain).to_vec())
}

/// Whether this browser remembers itself (a device key is kept).
pub async fn is_remembered() -> bool {
    matches!(get(DEVICE).await, Ok(Some(_)))
}

/// The disk key, under the device key: the one kept, or (when `create`) a new
/// device key and disk key, made and kept now. `None` when this browser does
/// not remember itself and `create` is false.
pub async fn disk_key(create: bool) -> io::Result<Option<pimble_crypto::SymmetricKey>> {
    let Some(device) = device_key(create).await? else { return Ok(None) };
    if let Some(record) = get(DISK).await? {
        let bytes = open_sealed(&device, &record).await?;
        let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the disk key is not 32 bytes"))?;
        return Ok(Some(pimble_crypto::SymmetricKey(key)));
    }
    let key = pimble_crypto::SymmetricKey::generate();
    put(DISK, &seal(&device, &key.0).await?).await?;
    Ok(Some(key))
}

/// Forget this browser: the device key first, then everything beside it (the
/// database deleted). OPFS is the caller's to clear after.
pub async fn forget() -> io::Result<()> {
    let first = delete(DEVICE).await;
    let factory: IdbFactory = global("indexedDB")?.unchecked_into();
    let request = factory.delete_database(DB).map_err(|e| failed("deleting the device database", e))?;
    answered(&request).await?;
    first
}

/// The keystore's resting place in a remembered browser: its bytes sealed
/// under the device key, in IndexedDB beside it.
#[derive(Debug, Default)]
pub struct DeviceKeyRest;

#[async_trait::async_trait(?Send)]
impl pimble_host::KeyRest for DeviceKeyRest {
    async fn load(&self) -> io::Result<Option<Vec<u8>>> {
        let Some(device) = device_key(false).await? else { return Ok(None) };
        match get(KEYSTORE).await? {
            Some(record) => open_sealed(&device, &record).await.map(Some),
            None => Ok(None),
        }
    }

    async fn save(&self, bytes: &[u8]) -> io::Result<()> {
        let device = device_key(true).await?.ok_or_else(|| io::Error::other("no device key"))?;
        put(KEYSTORE, &seal(&device, bytes).await?).await
    }

    fn describe(&self) -> String {
        "IndexedDB, sealed under this browser's device key".to_string()
    }
}

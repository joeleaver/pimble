//! A plain store's pictures: immutable blobs beside its documents
//! (docs/IMAGES_CONTRACT.md "On disk").
//!
//! ```text
//! <store>/blobs/<blob id>         a blob: header, then the image's bytes
//! <store>/blobs/.<blob id>.part   one being written (an upload in progress)
//! ```
//!
//! The header is the magic `PIMG`, a version byte, the MIME type (a length
//! byte and that many bytes), the image's length (`u64`, little-endian) and
//! its SHA-256. A blob is written under its `.part` name and renamed, so a
//! file named by an id is whole or absent; the hash is checked on every read,
//! and a file that fails it is an absent blob and a line in the log.
//!
//! A blob is never changed and never deleted while its store exists: undo,
//! history and "Put Back" can bring a reference back. What a store's text
//! names is read from the text (`NodeDoc::blob_refs`), never from here.
//!
//! Every operation works on the directory alone, so a [`BlobStore`] is a
//! cheap handle a caller clones out from under the store manager's lock and
//! does its reading and writing without holding up anyone else.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use pimble_core::{BlobId, ImageMime, StoreId, IMAGE_TOO_LARGE, MAX_IMAGE_BYTES, NOT_AN_IMAGE};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::error::{Result, StoreError};

const MAGIC: &[u8; 4] = b"PIMG";
const VERSION: u8 = 1;
const HASH_LEN: usize = 32;
const PART_SUFFIX: &str = ".part";

/// How long an upload may sit untouched before the next upload clears it
/// away. A 20 MiB picture is a handful of chunks sent one after another, so
/// an hour-old part belongs to a client that went away.
const STALE_UPLOAD: Duration = Duration::from_secs(60 * 60);

/// What a person is told when a picture with no bytes is put.
pub const EMPTY_IMAGE: &str = "A picture cannot be empty.";

/// A picture read from a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub mime: ImageMime,
    pub bytes: Vec<u8>,
}

/// Where an upload stands after one of its chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobUpload {
    /// The blob's id: minted with the first chunk, named by every later one.
    pub id: BlobId,
    /// How many of the image's bytes the store holds so far.
    pub received: u64,
    /// Whether that was the last chunk: the blob is in place and readable.
    pub complete: bool,
}

/// One store's blobs. See the module documentation.
#[derive(Debug, Clone)]
pub struct BlobStore {
    store_id: StoreId,
    dir: PathBuf,
    /// Held while a part file is appended to or finished, so two chunks of
    /// one upload arriving together cannot interleave.
    writing: Arc<Mutex<()>>,
}

/// The fixed fields of a blob file's header.
struct Header {
    mime: ImageMime,
    len: u64,
    hash: [u8; HASH_LEN],
}

impl Header {
    fn size(mime: ImageMime) -> usize {
        MAGIC.len() + 1 + 1 + mime.as_str().len() + 8 + HASH_LEN
    }

    fn encode(&self) -> Vec<u8> {
        let mime = self.mime.as_str().as_bytes();
        let mut out = Vec::with_capacity(Self::size(self.mime));
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(mime.len() as u8);
        out.extend_from_slice(mime);
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.hash);
        out
    }

    /// The header at the start of `bytes` and where the image begins; `None`
    /// for anything this version did not write.
    fn decode(bytes: &[u8]) -> Option<(Self, usize)> {
        let rest = bytes.strip_prefix(MAGIC)?;
        let (&version, rest) = rest.split_first()?;
        if version != VERSION {
            return None;
        }
        let (&mime_len, rest) = rest.split_first()?;
        let mime = rest.get(..mime_len as usize)?;
        let mime = ImageMime::parse(std::str::from_utf8(mime).ok()?)?;
        let rest = &rest[mime_len as usize..];
        let len = u64::from_le_bytes(rest.get(..8)?.try_into().ok()?);
        let hash: [u8; HASH_LEN] = rest.get(8..8 + HASH_LEN)?.try_into().ok()?;
        Some((Self { mime, len, hash }, Self::size(mime)))
    }

    /// The header of the file at `path`, without reading the image.
    fn read_from(path: &Path) -> Option<Self> {
        let mut file = fs::File::open(path).ok()?;
        // Longer than any header: the longest accepted MIME type is ten bytes.
        let mut head = [0u8; 128];
        let mut filled = 0;
        while filled < head.len() {
            match file.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        Self::decode(&head[..filled]).map(|(header, _)| header)
    }
}

fn refused(sentence: &str) -> StoreError {
    StoreError::BlobRefused(sentence.to_string())
}

/// The accepted type `mime` names, when an image of `len` bytes may be kept.
fn admit(mime: &str, len: u64) -> Result<ImageMime> {
    let mime = ImageMime::parse(mime).ok_or_else(|| refused(NOT_AN_IMAGE))?;
    if len > MAX_IMAGE_BYTES {
        return Err(refused(IMAGE_TOO_LARGE));
    }
    if len == 0 {
        return Err(refused(EMPTY_IMAGE));
    }
    Ok(mime)
}

/// The bytes must be what the type says: a cheap look at the first bytes,
/// which is what stops a script named `image/png` from being kept and served.
fn check_magic(mime: ImageMime, bytes: &[u8]) -> Result<()> {
    if ImageMime::sniff(bytes) == Some(mime) {
        Ok(())
    } else {
        Err(refused(NOT_AN_IMAGE))
    }
}

impl BlobStore {
    const DIR: &'static str = "blobs";

    /// The blobs of the store at `store_path`. Creates nothing: the directory
    /// appears with the first blob.
    pub fn new(store_id: StoreId, store_path: &Path) -> Self {
        Self { store_id, dir: store_path.join(Self::DIR), writing: Arc::new(Mutex::new(())) }
    }

    fn path(&self, id: BlobId) -> PathBuf {
        self.dir.join(id.to_string())
    }

    fn part_path(&self, id: BlobId) -> PathBuf {
        self.dir.join(format!(".{id}{PART_SUFFIX}"))
    }

    /// Run `work` off the async threads: every operation here is file IO, and
    /// hashing twenty megabytes is not something to do between two awaits.
    async fn blocking<T: Send + 'static>(&self, work: impl FnOnce(&BlobStore) -> Result<T> + Send + 'static) -> Result<T> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || work(&this))
            .await
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?
    }

    /// Keep `bytes` as a new blob and return its id. Refused, with a sentence
    /// a person can read, for a type outside [`ImageMime::ALL`], for bytes
    /// that are not that type, and for more than [`MAX_IMAGE_BYTES`].
    pub async fn put(&self, mime: &str, bytes: Vec<u8>) -> Result<BlobId> {
        let mime = admit(mime, bytes.len() as u64)?;
        self.blocking(move |this| {
            check_magic(mime, &bytes)?;
            let id = BlobId::new();
            this.write_whole(id, mime, &bytes)?;
            Ok(id)
        })
        .await
    }

    /// Keep `bytes` under an id minted somewhere else (a replica copying a
    /// blob from its remote). `Ok(false)` when the store already holds that
    /// blob with the same bytes, which changes nothing; an error when it
    /// holds different bytes under the id, since a blob is immutable. A file
    /// that fails its own check is absent, and is replaced.
    pub async fn put_as(&self, id: BlobId, mime: &str, bytes: Vec<u8>) -> Result<bool> {
        let mime = admit(mime, bytes.len() as u64)?;
        self.blocking(move |this| {
            check_magic(mime, &bytes)?;
            if let Some(held) = this.read(id) {
                if held.bytes == bytes {
                    return Ok(false);
                }
                return Err(StoreError::InvalidOperation(format!(
                    "Blob {id} is already in store {} with different contents.",
                    this.store_id
                )));
            }
            this.write_whole(id, mime, &bytes)?;
            Ok(true)
        })
        .await
    }

    /// The blob `id`, or `None` when the store does not hold it whole: no
    /// file, or one whose length or hash is not what its header says (logged).
    pub async fn get(&self, id: BlobId) -> Result<Option<Blob>> {
        self.blocking(move |this| Ok(this.read(id))).await
    }

    /// Whether the store holds `id`: a file whose header reads and whose
    /// length is what the header says. The hash is left to the read.
    pub async fn has(&self, id: BlobId) -> bool {
        self.blocking(move |this| Ok(this.holds(id))).await.unwrap_or(false)
    }

    /// Which of `ids` the store lacks, in the order given.
    pub async fn missing(&self, ids: &[BlobId]) -> Vec<BlobId> {
        let ids = ids.to_vec();
        let all = ids.clone();
        self.blocking(move |this| Ok(ids.into_iter().filter(|id| !this.holds(*id)).collect())).await.unwrap_or(all)
    }

    /// One chunk of a picture too large for one message. The first chunk
    /// (`upload: None`, `offset` 0) starts an upload and answers with the id
    /// the blob will have; each later chunk names that id and the offset it
    /// continues from, which must be exactly what the store holds so far. The
    /// chunk that brings the bytes to `total` finishes it: the whole image is
    /// checked against its type, hashed and renamed into place. Until then
    /// nothing is readable under the id, and an upload that is never finished
    /// leaves only a part file, removed when the store next opens.
    pub async fn upload_chunk(&self, upload: Option<BlobId>, mime: &str, total: u64, offset: u64, bytes: Vec<u8>) -> Result<BlobUpload> {
        let mime = admit(mime, total)?;
        if bytes.is_empty() {
            return Err(StoreError::InvalidOperation("A chunk of a picture cannot be empty.".into()));
        }
        if offset.checked_add(bytes.len() as u64).is_none_or(|end| end > total) {
            return Err(StoreError::InvalidOperation("A chunk of a picture runs past the length it was given.".into()));
        }
        self.blocking(move |this| this.write_chunk(upload, mime, total, offset, &bytes)).await
    }

    /// Remove what unfinished uploads left behind. Run when a store opens:
    /// nothing can be mid-upload then.
    pub async fn sweep_partial(&self) -> Result<usize> {
        self.blocking(|this| this.remove_parts(None)).await
    }

    // ── The synchronous halves ───────────────────────────────────────────

    fn holds(&self, id: BlobId) -> bool {
        let path = self.path(id);
        let Some(header) = Header::read_from(&path) else { return false };
        fs::metadata(&path).is_ok_and(|meta| meta.len() == Header::size(header.mime) as u64 + header.len)
    }

    fn read(&self, id: BlobId) -> Option<Blob> {
        let path = self.path(id);
        let file = match fs::read(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!("Store {}: blob {} cannot be read: {}", self.store_id, id, e);
                return None;
            }
        };
        let Some((header, start)) = Header::decode(&file) else {
            warn!("Store {}: blob {} has no header this version reads; treating it as absent", self.store_id, id);
            return None;
        };
        let bytes = &file[start..];
        if bytes.len() as u64 != header.len || Sha256::digest(bytes).as_slice() != header.hash {
            warn!("Store {}: blob {} does not match its own hash; treating it as absent", self.store_id, id);
            return None;
        }
        Some(Blob { mime: header.mime, bytes: bytes.to_vec() })
    }

    /// Write a whole blob: the part file, synced, then renamed into place.
    fn write_whole(&self, id: BlobId, mime: ImageMime, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let header = Header { mime, len: bytes.len() as u64, hash: Sha256::digest(bytes).into() };
        let part = self.part_path(id);
        let written = (|| {
            let mut file = fs::File::create(&part)?;
            file.write_all(&header.encode())?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&part, self.path(id))
        })();
        if written.is_err() {
            let _ = fs::remove_file(&part);
        }
        Ok(written?)
    }

    fn write_chunk(&self, upload: Option<BlobId>, mime: ImageMime, total: u64, offset: u64, bytes: &[u8]) -> Result<BlobUpload> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let out_of_step = || StoreError::InvalidOperation("This picture's upload is out of step; start it again.".into());
        let head = Header::size(mime) as u64;

        let id = match upload {
            None => {
                if offset != 0 {
                    return Err(out_of_step());
                }
                // Enough of the file to tell: refuse the wrong thing now
                // rather than after twenty megabytes of it.
                if bytes.len() as u64 == total || bytes.len() >= 4096 {
                    check_magic(mime, bytes)?;
                }
                fs::create_dir_all(&self.dir)?;
                self.remove_parts(Some(STALE_UPLOAD))?;
                let id = BlobId::new();
                // The hash is not known until the last byte: zeros until then.
                let header = Header { mime, len: total, hash: [0; HASH_LEN] };
                fs::File::create(self.part_path(id))?.write_all(&header.encode())?;
                id
            }
            Some(id) => id,
        };

        let part = self.part_path(id);
        let appended = (|| -> Result<u64> {
            // An upload that is not there (never started, swept, finished)
            // and one continued under another type or length are the same
            // mistake to the caller.
            let header = Header::read_from(&part).ok_or_else(out_of_step)?;
            if header.mime != mime || header.len != total {
                return Err(out_of_step());
            }
            let mut file = fs::OpenOptions::new().read(true).write(true).open(&part)?;
            if file.metadata()?.len() != head + offset {
                return Err(out_of_step());
            }
            file.seek(SeekFrom::End(0))?;
            file.write_all(bytes)?;
            let received = offset + bytes.len() as u64;
            if received < total {
                return Ok(received);
            }

            // The last chunk: check the whole image, write its hash into the
            // header, and only then give the file its name.
            file.seek(SeekFrom::Start(head))?;
            let mut image = Vec::with_capacity(total as usize);
            file.read_to_end(&mut image)?;
            check_magic(mime, &image)?;
            let hash: [u8; HASH_LEN] = Sha256::digest(&image).into();
            file.seek(SeekFrom::Start(head - HASH_LEN as u64))?;
            file.write_all(&hash)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&part, self.path(id))?;
            Ok(received)
        })();

        match appended {
            Ok(received) => Ok(BlobUpload { id, received, complete: received == total }),
            Err(e) => {
                // A refused or broken upload leaves nothing; an out-of-step
                // chunk leaves the upload it was not part of alone.
                if upload.is_none() || matches!(e, StoreError::BlobRefused(_) | StoreError::Io(_)) {
                    let _ = fs::remove_file(&part);
                }
                Err(e)
            }
        }
    }

    /// Remove part files: all of them, or those untouched for `older_than`.
    fn remove_parts(&self, older_than: Option<Duration>) -> Result<usize> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let now = SystemTime::now();
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().ends_with(PART_SUFFIX) {
                continue;
            }
            if let Some(age) = older_than {
                let modified = entry.metadata().and_then(|meta| meta.modified()).unwrap_or(now);
                if now.duration_since(modified).unwrap_or_default() < age {
                    continue;
                }
            }
            if fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            debug!("Store {}: removed {} unfinished picture upload(s)", self.store_id, removed);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The smallest thing that passes for a PNG here: the signature, then
    /// `len - 8` bytes that differ from one image to the next.
    fn png(len: usize, seed: u8) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend((0..len.saturating_sub(8)).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
        bytes
    }

    fn store() -> (TempDir, BlobStore) {
        let dir = TempDir::new().unwrap();
        let blobs = BlobStore::new(StoreId::new(), dir.path());
        (dir, blobs)
    }

    fn names(blobs: &BlobStore) -> Vec<String> {
        match fs::read_dir(&blobs.dir) {
            Ok(entries) => entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn sentence(result: Result<impl std::fmt::Debug>) -> String {
        result.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn a_blob_is_put_and_read_back_with_its_type() {
        let (_dir, blobs) = store();
        let image = png(5000, 1);
        let id = blobs.put("image/png", image.clone()).await.unwrap();
        assert_eq!(blobs.get(id).await.unwrap(), Some(Blob { mime: ImageMime::Png, bytes: image.clone() }));
        assert!(blobs.has(id).await);
        assert_eq!(names(&blobs), vec![id.to_string()], "one file, named by the id, and no part left");

        // On disk: the header the contract describes, then the bytes.
        let file = fs::read(blobs.path(id)).unwrap();
        assert_eq!(&file[..4], b"PIMG");
        assert_eq!(file[4], 1);
        assert_eq!(&file[6..6 + file[5] as usize], b"image/png");
        assert!(file.ends_with(&image));

        // The same picture again is a second blob.
        let again = blobs.put("image/png", image).await.unwrap();
        assert_ne!(again, id);

        let absent = BlobId::new();
        assert_eq!(blobs.get(absent).await.unwrap(), None);
        assert_eq!(blobs.missing(&[id, absent, again]).await, vec![absent]);
    }

    #[tokio::test]
    async fn what_is_not_an_accepted_image_is_refused_with_a_sentence() {
        let (_dir, blobs) = store();
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>".to_vec();
        assert_eq!(sentence(blobs.put("image/svg+xml", svg.clone()).await), NOT_AN_IMAGE);
        // An SVG that says it is a PNG, and a PNG that says it is a JPEG.
        assert_eq!(sentence(blobs.put("image/png", svg).await), NOT_AN_IMAGE);
        assert_eq!(sentence(blobs.put("image/jpeg", png(100, 0)).await), NOT_AN_IMAGE);
        assert_eq!(sentence(blobs.put("image/png", Vec::new()).await), EMPTY_IMAGE);
        assert_eq!(sentence(blobs.put("image/png", png(MAX_IMAGE_BYTES as usize + 1, 0)).await), IMAGE_TOO_LARGE);
        assert_eq!(IMAGE_TOO_LARGE, "Pictures can be at most 20 MiB.");
        assert!(names(&blobs).is_empty(), "a refusal writes nothing: {:?}", names(&blobs));

        // Exactly the limit is kept.
        let id = blobs.put("image/png", png(MAX_IMAGE_BYTES as usize, 0)).await.unwrap();
        assert!(blobs.has(id).await);
    }

    #[tokio::test]
    async fn a_file_that_fails_its_hash_is_an_absent_blob() {
        let (_dir, blobs) = store();
        let image = png(4000, 2);
        let id = blobs.put("image/png", image.clone()).await.unwrap();

        // One flipped bit in the image.
        let path = blobs.path(id);
        let mut file = fs::read(&path).unwrap();
        let last = file.len() - 1;
        file[last] ^= 1;
        fs::write(&path, &file).unwrap();
        assert_eq!(blobs.get(id).await.unwrap(), None);

        // Cut short: not even held.
        fs::write(&path, &file[..file.len() / 2]).unwrap();
        assert!(!blobs.has(id).await);
        assert_eq!(blobs.get(id).await.unwrap(), None);
        assert_eq!(blobs.missing(&[id]).await, vec![id]);

        // Not a blob file at all.
        fs::write(&path, b"junk").unwrap();
        assert_eq!(blobs.get(id).await.unwrap(), None);

        // An absent blob can be put again under its id.
        assert!(blobs.put_as(id, "image/png", image.clone()).await.unwrap());
        assert_eq!(blobs.get(id).await.unwrap().unwrap().bytes, image);
    }

    #[tokio::test]
    async fn putting_under_a_given_id_is_a_no_op_for_the_same_bytes_and_an_error_for_others() {
        let (_dir, blobs) = store();
        let id = BlobId::new();
        let image = png(3000, 3);
        assert!(blobs.put_as(id, "image/png", image.clone()).await.unwrap());
        let written = fs::metadata(blobs.path(id)).unwrap().modified().unwrap();
        assert!(!blobs.put_as(id, "image/png", image.clone()).await.unwrap());
        assert_eq!(fs::metadata(blobs.path(id)).unwrap().modified().unwrap(), written, "the file was not rewritten");

        let err = blobs.put_as(id, "image/png", png(3000, 4)).await.unwrap_err();
        assert!(err.to_string().contains("different contents"), "{err}");
        assert_eq!(blobs.get(id).await.unwrap().unwrap().bytes, image, "the first bytes stay");
        assert_eq!(sentence(blobs.put_as(BlobId::new(), "image/svg+xml", image).await), NOT_AN_IMAGE);
    }

    #[tokio::test]
    async fn a_chunked_upload_is_readable_only_once_its_last_chunk_arrives() {
        let (_dir, blobs) = store();
        let image = png(10_000, 5);
        let total = image.len() as u64;

        let first = blobs.upload_chunk(None, "image/png", total, 0, image[..4096].to_vec()).await.unwrap();
        assert_eq!((first.received, first.complete), (4096, false));
        assert!(!blobs.has(first.id).await);
        assert_eq!(blobs.get(first.id).await.unwrap(), None);
        assert_eq!(names(&blobs), vec![format!(".{}.part", first.id)]);

        // A chunk from the wrong place changes nothing and loses nothing.
        for offset in [0, 4000, 5000] {
            let err = blobs.upload_chunk(Some(first.id), "image/png", total, offset, image[4096..8000].to_vec()).await.unwrap_err();
            assert!(err.to_string().contains("out of step"), "{err}");
        }
        // Nor does one that changes its story.
        assert!(blobs.upload_chunk(Some(first.id), "image/png", total + 1, 4096, image[4096..8000].to_vec()).await.is_err());
        assert!(blobs.upload_chunk(Some(first.id), "image/gif", total, 4096, image[4096..8000].to_vec()).await.is_err());
        assert!(blobs.upload_chunk(Some(first.id), "image/png", total, 4096, image[4096..].iter().chain(&[0u8]).copied().collect()).await.is_err());

        let second = blobs.upload_chunk(Some(first.id), "image/png", total, 4096, image[4096..8000].to_vec()).await.unwrap();
        assert_eq!((second.id, second.received, second.complete), (first.id, 8000, false));
        let last = blobs.upload_chunk(Some(first.id), "image/png", total, 8000, image[8000..].to_vec()).await.unwrap();
        assert_eq!((last.received, last.complete), (total, true));

        assert_eq!(blobs.get(first.id).await.unwrap(), Some(Blob { mime: ImageMime::Png, bytes: image.clone() }));
        assert_eq!(names(&blobs), vec![first.id.to_string()]);
        // A finished upload takes no more.
        assert!(blobs.upload_chunk(Some(first.id), "image/png", total, total - 1, vec![0]).await.is_err());
        // An upload nobody started.
        assert!(blobs.upload_chunk(Some(BlobId::new()), "image/png", total, 4096, image[4096..].to_vec()).await.is_err());
    }

    #[tokio::test]
    async fn an_upload_is_refused_for_its_size_at_the_first_chunk_and_for_its_bytes_at_the_last() {
        let (_dir, blobs) = store();
        let over = MAX_IMAGE_BYTES + 1;
        assert_eq!(sentence(blobs.upload_chunk(None, "image/png", over, 0, png(4096, 0)).await), IMAGE_TOO_LARGE);
        assert_eq!(sentence(blobs.upload_chunk(None, "image/svg+xml", 9000, 0, png(4096, 0)).await), NOT_AN_IMAGE);
        // A first chunk large enough to tell is checked at once.
        assert_eq!(sentence(blobs.upload_chunk(None, "image/png", 9000, 0, vec![b'<'; 4096]).await), NOT_AN_IMAGE);
        assert!(names(&blobs).is_empty(), "{:?}", names(&blobs));

        // One too small to tell is caught when the image is whole.
        let first = blobs.upload_chunk(None, "image/png", 200, 0, vec![b'<'; 100]).await.unwrap();
        assert_eq!(sentence(blobs.upload_chunk(Some(first.id), "image/png", 200, 100, vec![b'>'; 100]).await), NOT_AN_IMAGE);
        assert!(names(&blobs).is_empty(), "a refused upload leaves nothing: {:?}", names(&blobs));
    }

    #[tokio::test]
    async fn an_abandoned_upload_is_swept() {
        let (_dir, blobs) = store();
        let kept = blobs.put("image/png", png(2000, 6)).await.unwrap();
        let image = png(10_000, 7);
        let abandoned = blobs.upload_chunk(None, "image/png", image.len() as u64, 0, image[..5000].to_vec()).await.unwrap();
        assert_eq!(names(&blobs).len(), 2);

        // A new upload does not disturb one that is still fresh.
        let other = blobs.upload_chunk(None, "image/png", image.len() as u64, 0, image[..5000].to_vec()).await.unwrap();
        assert!(blobs.part_path(abandoned.id).exists());

        assert_eq!(blobs.sweep_partial().await.unwrap(), 2);
        assert_eq!(names(&blobs), vec![kept.to_string()]);
        assert!(blobs.has(kept).await);
        assert!(blobs.upload_chunk(Some(other.id), "image/png", image.len() as u64, 5000, image[5000..].to_vec()).await.is_err());
        assert_eq!(names(&blobs), vec![kept.to_string()]);

        // A store with no blobs directory sweeps nothing.
        let (_dir, empty) = store();
        assert_eq!(empty.sweep_partial().await.unwrap(), 0);
    }
}

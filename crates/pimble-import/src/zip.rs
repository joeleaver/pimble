//! Reading a zip archive, as much of the format as a .docx needs.
//!
//! A Word document is a zip of XML parts. This reads the archive's central
//! directory (found through the End Of Central Directory record at the tail,
//! which may be followed by a comment) and hands out one entry's bytes by name,
//! stored or deflated. Zip64, encryption, multi-disk archives and other
//! compression methods are refused with an error: no word processor writes them
//! for a document. The input is untrusted, so every offset and length is checked
//! against the bytes before it is used, and an entry inflates to at most
//! [`MAX_ENTRY_SIZE`], so a zip bomb is an error rather than all the memory there
//! is. Nothing here touches the disk, so it builds for the browser.

use anyhow::{anyhow, bail, Result};

/// The most one entry may inflate to. A novel's `document.xml` is a few
/// megabytes; anything near this is not a document anyone wrote.
pub(crate) const MAX_ENTRY_SIZE: usize = 64 * 1024 * 1024;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_LOCATOR_LEN: usize = 20;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const CENTRAL_LEN: usize = 46;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const LOCAL_LEN: usize = 30;

/// One file in the archive, as the central directory describes it.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    method: u16,
    flags: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    local_offset: usize,
}

/// An archive read from bytes in memory.
pub(crate) struct Archive<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    /// Read the central directory of `bytes`. Errors when `bytes` is not a zip
    /// archive, or is one this reader does not handle (zip64, split archives).
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self> {
        let eocd = find_eocd(bytes).ok_or_else(|| anyhow!("no end of central directory record"))?;
        if eocd >= ZIP64_LOCATOR_LEN && u32_at(bytes, eocd - ZIP64_LOCATOR_LEN) == Some(ZIP64_LOCATOR_SIGNATURE) {
            bail!("zip64 archives are not supported");
        }
        let field16 = |at: usize| u16_at(bytes, eocd + at).ok_or_else(|| anyhow!("truncated end record"));
        let field32 = |at: usize| u32_at(bytes, eocd + at).ok_or_else(|| anyhow!("truncated end record"));
        let disk = field16(4)?;
        let cd_disk = field16(6)?;
        let entries_here = field16(8)?;
        let entries_total = field16(10)?;
        let cd_size = field32(12)?;
        let cd_offset = field32(16)?;
        if entries_total == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
            bail!("zip64 archives are not supported");
        }
        if disk != 0 || cd_disk != 0 || entries_here != entries_total {
            bail!("split archives are not supported");
        }
        let cd_start = cd_offset as usize;
        let cd_end = cd_start.checked_add(cd_size as usize).ok_or_else(|| anyhow!("central directory out of range"))?;
        if cd_end > eocd {
            bail!("central directory out of range");
        }

        let mut entries = Vec::with_capacity(entries_total as usize);
        let mut at = cd_start;
        for _ in 0..entries_total {
            let header = bytes.get(at..at + CENTRAL_LEN).filter(|_| at + CENTRAL_LEN <= cd_end);
            let Some(header) = header else { bail!("central directory truncated") };
            if u32_at(header, 0) != Some(CENTRAL_SIGNATURE) {
                bail!("bad central directory entry");
            }
            let h16 = |o: usize| u16_at(header, o).unwrap_or(0) as usize;
            let h32 = |o: usize| u32_at(header, o).unwrap_or(0);
            let flags = h16(8) as u16;
            let method = h16(10) as u16;
            let compressed_size = h32(20);
            let uncompressed_size = h32(24);
            let name_len = h16(28);
            let extra_len = h16(30);
            let comment_len = h16(32);
            let local_offset = h32(42);
            if compressed_size == 0xFFFF_FFFF || uncompressed_size == 0xFFFF_FFFF || local_offset == 0xFFFF_FFFF {
                bail!("zip64 archives are not supported");
            }
            let name_start = at + CENTRAL_LEN;
            let name = bytes
                .get(name_start..name_start + name_len)
                .ok_or_else(|| anyhow!("central directory truncated"))?;
            // Some writers use backslashes; the parts are named with slashes.
            let name = String::from_utf8_lossy(name).replace('\\', "/");
            entries.push(Entry {
                name,
                method,
                flags,
                compressed_size: compressed_size as usize,
                uncompressed_size: uncompressed_size as usize,
                local_offset: local_offset as usize,
            });
            at = name_start + name_len + extra_len + comment_len;
            if at > cd_end {
                bail!("central directory truncated");
            }
        }
        Ok(Archive { bytes, entries })
    }

    /// The bytes of the entry called `name` (a path inside the archive such as
    /// `word/document.xml`), `None` when there is no such entry. The name is
    /// matched exactly first and then ignoring ASCII case, since some writers
    /// disagree with the relationships about case.
    pub(crate) fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let name = name.trim_start_matches('/');
        let entry = self
            .entries
            .iter()
            .find(|e| e.name == name)
            .or_else(|| self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)));
        match entry {
            Some(entry) => self.extract(entry).map(Some),
            None => Ok(None),
        }
    }

    fn extract(&self, entry: &Entry) -> Result<Vec<u8>> {
        if entry.flags & 1 != 0 {
            bail!("\"{}\" is encrypted", entry.name);
        }
        if entry.uncompressed_size > MAX_ENTRY_SIZE {
            bail!("\"{}\" is too large", entry.name);
        }
        // The local header repeats the name and has its own extra field, which may
        // differ in length from the central directory's: read its lengths from it.
        let local = entry.local_offset;
        let header = self
            .bytes
            .get(local..local.checked_add(LOCAL_LEN).ok_or_else(|| anyhow!("entry out of range"))?)
            .ok_or_else(|| anyhow!("\"{}\" is out of range", entry.name))?;
        if u32_at(header, 0) != Some(LOCAL_SIGNATURE) {
            bail!("bad local header for \"{}\"", entry.name);
        }
        let name_len = u16_at(header, 26).unwrap_or(0) as usize;
        let extra_len = u16_at(header, 28).unwrap_or(0) as usize;
        let data_start = local + LOCAL_LEN + name_len + extra_len;
        let data = data_start
            .checked_add(entry.compressed_size)
            .and_then(|end| self.bytes.get(data_start..end))
            .ok_or_else(|| anyhow!("\"{}\" is truncated", entry.name))?;
        match entry.method {
            0 => {
                if data.len() != entry.uncompressed_size {
                    bail!("\"{}\" has inconsistent sizes", entry.name);
                }
                Ok(data.to_vec())
            }
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_ENTRY_SIZE)
                .map_err(|e| anyhow!("\"{}\" could not be inflated ({:?})", entry.name, e.status)),
            other => bail!("\"{}\" uses compression method {other}, which is not supported", entry.name),
        }
    }
}

/// Where the End Of Central Directory record starts: searched for backwards from
/// the end, since up to 64 KiB of comment may follow it. A candidate counts only
/// when its comment length reaches exactly to the end of the bytes, so a
/// signature inside the comment or the data is not mistaken for it.
fn find_eocd(bytes: &[u8]) -> Option<usize> {
    let last = bytes.len().checked_sub(EOCD_LEN)?;
    let first = last.saturating_sub(u16::MAX as usize);
    (first..=last).rev().find(|&at| {
        u32_at(bytes, at) == Some(EOCD_SIGNATURE)
            && u16_at(bytes, at + 20).map_or(false, |comment| at + EOCD_LEN + comment as usize == bytes.len())
    })
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A zip writer for tests: each entry stored, or deflated when its flag says so.
/// CRCs are left zero, since the reader does not check them.
#[cfg(test)]
pub(crate) fn write_zip(entries: &[(&str, &[u8], bool)], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in entries {
        let body = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
        let method: u16 = if *deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // time, date
        out.extend_from_slice(&[0; 4]); // crc
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        // An extra field in the local header only, to prove its length is read from there.
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&[0xAA, 0xBB, 0, 0]);
        out.extend_from_slice(&body);

        central.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]); // time, date, crc
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]); // extra, comment, disk, internal and external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_deflated_entries_read_back() {
        let big = "word ".repeat(1000);
        let zip = write_zip(&[("a.txt", b"hello", false), ("dir/b.xml", big.as_bytes(), true)], b"");
        let archive = Archive::open(&zip).unwrap();
        assert_eq!(archive.read("a.txt").unwrap().unwrap(), b"hello");
        assert_eq!(archive.read("dir/b.xml").unwrap().unwrap(), big.as_bytes());
        assert_eq!(archive.read("/DIR/B.XML").unwrap().unwrap(), big.as_bytes());
        assert!(archive.read("missing").unwrap().is_none());
    }

    #[test]
    fn a_trailing_comment_is_allowed() {
        let zip = write_zip(&[("a.txt", b"hello", false)], b"a comment with PK\x05\x06 in it");
        let archive = Archive::open(&zip).unwrap();
        assert_eq!(archive.read("a.txt").unwrap().unwrap(), b"hello");
    }

    #[test]
    fn malformed_input_errors_without_panicking() {
        assert!(Archive::open(b"").is_err());
        assert!(Archive::open(b"not a zip at all, just words").is_err());
        let zip = write_zip(&[("a.txt", b"hello there", true), ("b.txt", b"more", false)], b"");
        // Every truncation, and every single-byte corruption, either errors or reads.
        for len in 0..zip.len() {
            if let Ok(archive) = Archive::open(&zip[..len]) {
                let _ = archive.read("a.txt");
            }
        }
        for at in 0..zip.len() {
            for value in [0x00, 0xFF, 0x7F] {
                let mut bad = zip.clone();
                bad[at] = value;
                if let Ok(archive) = Archive::open(&bad) {
                    let _ = archive.read("a.txt");
                    let _ = archive.read("b.txt");
                }
            }
        }
        // Pseudo-random bytes.
        let mut state = 0x1234_5678u32;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        assert!(Archive::open(&noise).is_err());
    }

    #[test]
    fn a_zip_bomb_is_an_error() {
        let huge = vec![0u8; MAX_ENTRY_SIZE + 1];
        let zip = write_zip(&[("bomb", &huge, true)], b"");
        let archive = Archive::open(&zip).unwrap();
        assert!(archive.read("bomb").is_err());
    }

    #[test]
    fn a_lying_size_does_not_get_past_the_limit() {
        let huge = vec![0u8; MAX_ENTRY_SIZE + 1];
        let mut zip = write_zip(&[("bomb", &huge, true)], b"");
        // Claim a small uncompressed size in the central directory.
        let cd = zip.len() - EOCD_LEN - (CENTRAL_LEN + 4);
        assert_eq!(u32_at(&zip, cd), Some(CENTRAL_SIGNATURE));
        zip[cd + 24..cd + 28].copy_from_slice(&10u32.to_le_bytes());
        let archive = Archive::open(&zip).unwrap();
        assert!(archive.read("bomb").is_err());
    }
}

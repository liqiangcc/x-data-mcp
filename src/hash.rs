//! Domain-independent byte digests.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

/// Streaming hash buffer. Every official artifact that must be hashed
/// (the day archive is ~550 MB) is read through this, so the digest cost is
/// a fixed 64 KiB buffer instead of one allocation the size of the file.
pub const STREAM_BUFFER: usize = 64 * 1024;

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// sha256 of every byte a reader yields, plus the byte count. Reads are
/// bounded by [`STREAM_BUFFER`], so a caller may hand it the largest
/// artifact without materializing it.
pub fn digest_reader(mut reader: impl Read) -> std::io::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; STREAM_BUFFER];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("digest byte count overflow"))?;
        hash.update(&buffer[..read]);
    }
    Ok((bytes, hex(&hash.finalize())))
}

/// [`digest_reader`] for a file path. The whole file is never held in
/// memory; a missing or unreadable file is an error, never an empty digest.
pub fn digest_file(path: &Path) -> std::io::Result<(u64, String)> {
    digest_reader(std::fs::File::open(path)?)
}

/// Copy every byte of `reader` into `writer` while hashing it, returning the
/// copied byte count and digest. One streaming pass, one fixed buffer: the
/// acquire mirror path copies the ~550 MB day archive this way instead of
/// reading it into memory first (XDM-81).
pub fn copy_digest(reader: impl Read, writer: &mut impl Write) -> std::io::Result<(u64, String)> {
    let mut tee = Tee {
        inner: reader,
        writer,
    };
    digest_reader(&mut tee)
}

/// Read-through writer: bytes counted by [`digest_reader`] are written on
/// the way past, so a copy never grows the resident set with file size.
struct Tee<'a, R, W> {
    inner: R,
    writer: &'a mut W,
}
impl<R: Read, W: Write> Read for Tee<'_, R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.writer.write_all(&buffer[..read])?;
        Ok(read)
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Mechanical format check matching what `sha256` produces.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that fails the test if it is ever handed a buffer larger
    /// than the streaming bound — the property the 550 MB archive path
    /// depends on (#383 XDM-81).
    struct BoundedReader {
        bytes: Vec<u8>,
        offset: usize,
    }

    impl Read for BoundedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                buffer.len() <= STREAM_BUFFER,
                "digest asked for a {} byte buffer",
                buffer.len()
            );
            let remaining = &self.bytes[self.offset..];
            let take = remaining.len().min(buffer.len());
            buffer[..take].copy_from_slice(&remaining[..take]);
            self.offset += take;
            Ok(take)
        }
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// The streaming digest is the in-memory digest for payloads below,
    /// at and above the buffer bound, including a partial final read.
    #[test]
    fn streaming_digest_matches_in_memory_digest() {
        for len in [
            0,
            1,
            STREAM_BUFFER - 1,
            STREAM_BUFFER,
            STREAM_BUFFER + 1,
            300_000,
        ] {
            let bytes = payload(len);
            let (counted, digest) = digest_reader(BoundedReader {
                bytes: bytes.clone(),
                offset: 0,
            })
            .unwrap();
            assert_eq!(counted, len as u64, "byte count for {len} bytes");
            assert_eq!(digest, sha256(&bytes), "digest for {len} bytes");
        }
    }

    /// A file digest equals the in-memory digest of the same bytes and
    /// reports the file length; a missing file fails instead of hashing
    /// nothing.
    #[test]
    fn file_digest_matches_its_bytes_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.bin");
        let bytes = payload(2 * STREAM_BUFFER + 17);
        std::fs::write(&path, &bytes).unwrap();
        let (counted, digest) = digest_file(&path).unwrap();
        assert_eq!(counted, bytes.len() as u64);
        assert_eq!(digest, sha256(&bytes));

        let missing = dir.path().join("absent.bin");
        assert!(digest_file(&missing).is_err());
    }

    /// A streamed copy is byte-identical to its source and reports the same
    /// count and digest the in-memory path would.
    #[test]
    fn streamed_copy_writes_every_byte_and_the_same_digest() {
        let bytes = payload(2 * STREAM_BUFFER + 5);
        let mut written = Vec::new();
        let (counted, digest) = copy_digest(&bytes[..], &mut written).unwrap();
        assert_eq!(counted, bytes.len() as u64);
        assert_eq!(written, bytes);
        assert_eq!(digest, sha256(&bytes));
    }
}

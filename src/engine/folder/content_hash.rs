//! The hash of a local file's content, of the kind its provider keeps of its copy ([`HashKind`]), so the sync can tell whether both hold the same content.
//!
//! - SHA-1: Google Drive, Nextcloud, Proton.
//! - Dropbox's content hash: the SHA-256 of the SHA-256s of the file's 4 MiB blocks.
//! - OneDrive's QuickXorHash: the bytes are XORed into a 160-bit value, each byte 11 bits further than the one before, then the length is XORed into the last 64 bits This follows rclone's implementation (backend/onedrive/quickxorhash), whose test vectors the tests use.
//! - MEGA's fingerprint CRCs ([`super::mega_crc`]): read from sampled windows of the file rather than from all of it.

use std::{fs, io::Read, path::Path};

use sha1::Digest;

use super::mega_crc;
use crate::domain::sync::{ContentHash, HashKind};

/// The hash of the file at `path` of the kind of `remote`, the hash of its remote copy: that one when the file matches it in any variant the provider writes, as MEGA's older CRC of large files.
pub fn matching(path: &Path, remote: &ContentHash) -> Option<ContentHash> {
    let local = of_file(path, remote.kind)?;
    if local == *remote || remote.kind != HashKind::Mega {
        return Some(local);
    }
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let legacy = mega_crc::legacy(&mut file, size).ok().flatten();
    Some(if legacy.is_some_and(|crc| hex(&crc) == remote.value) {
        remote.clone()
    } else {
        local
    })
}

/// The hash of kind `kind` of the file at `path`; `None` when it can't be read.
pub fn of_file(path: &Path, kind: HashKind) -> Option<ContentHash> {
    let mut file = fs::File::open(path).ok()?;
    if kind == HashKind::Mega {
        let size = file.metadata().ok()?.len();
        let crc = mega_crc::of(&mut file, size).ok()?;
        return Some(ContentHash {
            kind,
            value: hex(&crc),
        });
    }
    let mut hasher = Hasher::new(kind);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(ContentHash {
        kind,
        value: hex(&hasher.finish()),
    })
}

enum Hasher {
    Sha1(sha1::Sha1),
    Dropbox(DropboxHasher),
    QuickXor(Box<QuickXorHasher>),
}

impl Hasher {
    fn new(kind: HashKind) -> Self {
        match kind {
            HashKind::Sha1 => Self::Sha1(sha1::Sha1::new()),
            HashKind::Dropbox => Self::Dropbox(DropboxHasher::default()),
            HashKind::QuickXor => Self::QuickXor(Box::default()),
            HashKind::Mega => unreachable!("of_file reads MEGA's CRCs from windows of the file"),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(bytes),
            Self::Dropbox(h) => h.update(bytes),
            Self::QuickXor(h) => h.update(bytes),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Dropbox(h) => h.finish().to_vec(),
            Self::QuickXor(h) => h.finish().to_vec(),
        }
    }
}

/// Dropbox's content hash.
#[derive(Default)]
struct DropboxHasher {
    /// The SHA-256s of the finished blocks.
    overall: sha2::Sha256,
    block: sha2::Sha256,
    /// Bytes in `block` so far.
    filled: usize,
}

impl DropboxHasher {
    const BLOCK: usize = 4 * 1024 * 1024;

    fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let take = bytes.len().min(Self::BLOCK - self.filled);
            self.block.update(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == Self::BLOCK {
                self.finish_block();
            }
        }
    }

    fn finish_block(&mut self) {
        let block = std::mem::take(&mut self.block).finalize();
        self.overall.update(block);
        self.filled = 0;
    }

    fn finish(mut self) -> [u8; 32] {
        if self.filled > 0 {
            self.finish_block();
        }
        self.overall.finalize().into()
    }
}

/// OneDrive's QuickXorHash. Byte `i` of the content lands at bit `11 i` of the 160-bit value; as 160 × 11 bits pass before a bit position comes round again, the bytes are first XORed into 1760 cells by `i mod 1760`, each cell standing for one position.
struct QuickXorHasher {
    cells: [u8; Self::CELLS],
    len: u64,
}

impl Default for QuickXorHasher {
    fn default() -> Self {
        Self {
            cells: [0; Self::CELLS],
            len: 0,
        }
    }
}

impl QuickXorHasher {
    const WIDTH: usize = 160;
    const SHIFT: usize = 11;
    const CELLS: usize = Self::WIDTH * Self::SHIFT;

    fn update(&mut self, bytes: &[u8]) {
        let mut at = (self.len % Self::CELLS as u64) as usize;
        for byte in bytes {
            self.cells[at] ^= byte;
            at = (at + 1) % Self::CELLS;
        }
        self.len += bytes.len() as u64;
    }

    fn finish(self) -> [u8; 20] {
        // One byte more than the 160 bits, for the bits a byte at the end pushes over; they wrap round to the start.
        let mut value = [0u8; Self::WIDTH / 8 + 1];
        for (i, cell) in self.cells.iter().enumerate() {
            let bit = (i * Self::SHIFT) % Self::WIDTH;
            let shifted = u16::from(*cell) << (bit % 8);
            value[bit / 8] ^= shifted as u8;
            value[bit / 8 + 1] ^= (shifted >> 8) as u8;
        }
        value[0] ^= value[Self::WIDTH / 8];
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&value[..20]);
        for (h, l) in hash[12..].iter_mut().zip(self.len.to_le_bytes()) {
            *h ^= l;
        }
        hash
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(kind: HashKind, bytes: &[u8]) -> String {
        let mut hasher = Hasher::new(kind);
        // In uneven pieces, so a block or cell boundary falls inside one.
        for piece in bytes.chunks(1000) {
            hasher.update(piece);
        }
        hex(&hasher.finish())
    }

    /// The content the expected values below were computed for with rclone's hashers: `n` bytes of a pattern without a period of 1760 or 4 MiB.
    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 31 + i / 7) % 251) as u8).collect()
    }

    #[test]
    fn quick_xor_matches_rclones_test_vectors() {
        assert_eq!(
            hash(HashKind::QuickXor, b""),
            "0000000000000000000000000000000000000000"
        );
        assert_eq!(
            hash(HashKind::QuickXor, b"J"),
            "4a00000000000000000000000100000000000000"
        );
        assert_eq!(
            hash(HashKind::QuickXor, &[210, 150, 79]),
            "d2b0c41300000000000000000300000000000000"
        );
        assert_eq!(
            hash(
                HashKind::QuickXor,
                &[
                    217, 251, 177, 132, 18, 87, 182, 149, 133, 123, 199, 78, 125, 209, 158, 31
                ]
            ),
            "3bdb472c0821812bd8a21285c8c3319cd0876878"
        );
        assert_eq!(
            hash(
                HashKind::QuickXor,
                &[
                    210, 126, 231, 151, 118, 9, 182, 42, 114, 235, 39, 148, 109, 83, 214, 181, 205,
                    161, 227, 149, 155, 23, 219, 188, 133, 60, 249, 180, 220, 35, 221, 214, 89,
                    193, 245, 214, 146, 79, 134, 55, 120
                ]
            ),
            "6092cd2bb251eb88fd68e0d67ea0ef11efeee8d5"
        );
    }

    #[test]
    fn long_content_hashes_as_rclone_does() {
        let short = pattern(5000);
        let long = pattern(9 * 1024 * 1024 + 7);
        assert_eq!(
            hash(HashKind::QuickXor, &short),
            "9c621c087f49e4a6c571d3d261534da1672cd029"
        );
        assert_eq!(
            hash(HashKind::QuickXor, &long),
            "a3780e0e2166b1b54040937a9590b0e37153d1ee"
        );
        assert_eq!(
            hash(HashKind::Dropbox, &short),
            "67642c0febc9155a81697dca2ab46e4d093722a8d8a8233d17497732b7baa9eb"
        );
        assert_eq!(
            hash(HashKind::Dropbox, &long),
            "1b5cffe5805a2ba74de64ec040248772ea78aed0dfb41bb0d27a9483d24f811c"
        );
    }

    #[test]
    fn dropbox_hashes_a_single_block_twice() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            hash(HashKind::Dropbox, b"J"),
            hex(&Sha256::digest(Sha256::digest(b"J")))
        );
        assert_eq!(hash(HashKind::Dropbox, b""), hex(&Sha256::digest(b"")));
    }

    #[test]
    fn files_get_the_hash_with_its_kind() {
        let dir = crate::test_support::TempDir::new("content_hash");
        let path = dir.write_file("j.txt", b"J");
        assert_eq!(
            of_file(&path, HashKind::Sha1),
            Some(ContentHash {
                kind: HashKind::Sha1,
                value: "58668e7669fd564d99db5d581fcdb6a5618440b5".to_owned()
            })
        );
        assert_eq!(of_file(&dir.path.join("missing"), HashKind::Sha1), None);
    }

    #[test]
    fn a_mega_copy_with_the_legacy_crc_matches() {
        let dir = crate::test_support::TempDir::new("content_hash_mega");
        // 40 MiB as MEGA's SDK test fills them; the two CRCs are that test's.
        let mut x: u32 = 0xA5A5_A5A5;
        let data: Vec<u8> = (0..40 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let path = dir.write_file("big", &data);
        let mega = |value: &str| ContentHash {
            kind: HashKind::Mega,
            value: value.to_owned(),
        };
        let current = mega("ea2aa9532ec37402b1e4d211837d62fe");
        let legacy = mega("ea2aa9532ec37402b1e4d211197d4000");
        assert_eq!(of_file(&path, HashKind::Mega), Some(current.clone()));
        assert_eq!(matching(&path, &current), Some(current.clone()));
        assert_eq!(matching(&path, &legacy), Some(legacy));
        assert_eq!(matching(&path, &mega("00")), Some(current));
    }
}

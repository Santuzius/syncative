//! The content part of MEGA's file fingerprint, which MEGA keeps of every file: four CRC32s, so the sync can tell whether a local file and its MEGA copy hold the same content.
//!
//! - Files up to 16 bytes: the bytes themselves, padded with zeros.
//! - Files up to 8192 bytes: one CRC32 over each quarter of the file.
//! - Larger files are sampled: each CRC32 covers 32 windows of 64 bytes, window k (0…127 over all four) at offset (size − 64) · k / 127.
//!
//! Each CRC32 is stored in network byte order. MEGA's SDK computed the window offsets with 32-bit arithmetic until January 2026, which overflows for files over about 33.8 MB; files uploaded before then carry that variant ([`legacy`]). go-mega computes the same in Go (package `fingerprint` of the fork github.com/Santuzius/go-mega); both are tested against the values of MEGA's SDK tests.
//!
//! Source: MEGA SDK, src/filefingerprint.cpp and src/utils.cpp (legacySparseOffset32Bug), https://github.com/meganz/sdk.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

pub type Crc = [u8; 16];

const LANES: u64 = 4;
const MAX_FULL: u64 = 8192;
const WINDOW: u64 = 64;
const WINDOWS_PER_LANE: u64 = MAX_FULL / (WINDOW * LANES);
const DENOM: u64 = LANES * WINDOWS_PER_LANE - 1;
/// The smallest size whose legacy offsets can differ.
const LEGACY_MIN_SIZE: u64 = u32::MAX as u64 / DENOM + WINDOW;

/// The CRC of `file`, `size` bytes long.
pub fn of(file: &mut File, size: u64) -> io::Result<Crc> {
    crc(file, size, offset)
}

/// The CRC MEGA's SDK computed before January 2026, where it differs from [`of`]'s: only for files over about 33.8 MB.
pub fn legacy(file: &mut File, size: u64) -> io::Result<Option<Crc>> {
    if size < LEGACY_MIN_SIZE {
        return Ok(None);
    }
    crc(file, size, legacy_offset).map(Some)
}

fn crc(file: &mut File, size: u64, at: fn(u64, u64) -> u64) -> io::Result<Crc> {
    let mut crc = [0u8; 16];
    if size <= MAX_FULL {
        let mut data = vec![0u8; size as usize];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut data)?;
        if data.len() <= crc.len() {
            crc[..data.len()].copy_from_slice(&data);
            return Ok(crc);
        }
        let len = data.len();
        for lane in 0..LANES as usize {
            let quarter = &data[lane * len / 4..(lane + 1) * len / 4];
            crc[lane * 4..][..4].copy_from_slice(&crc32fast::hash(quarter).to_be_bytes());
        }
        return Ok(crc);
    }
    let mut window = [0u8; WINDOW as usize];
    for lane in 0..LANES {
        let mut hasher = crc32fast::Hasher::new();
        for j in 0..WINDOWS_PER_LANE {
            file.seek(SeekFrom::Start(at(size, lane * WINDOWS_PER_LANE + j)))?;
            file.read_exact(&mut window)?;
            hasher.update(&window);
        }
        crc[lane as usize * 4..][..4].copy_from_slice(&hasher.finalize().to_be_bytes());
    }
    Ok(crc)
}

/// Where window `k` (0…127) of a file of `size` bytes starts.
fn offset(size: u64, k: u64) -> u64 {
    let last = size - WINDOW;
    (last * k / DENOM).min(last)
}

/// [`offset`] as MEGA's SDK computed it before January 2026: with the size cut to 32 bits and the product wrapping at 2³².
fn legacy_offset(size: u64, k: u64) -> u64 {
    let at = (size as u32)
        .wrapping_sub(WINDOW as u32)
        .wrapping_mul(k as u32)
        / DENOM as u32;
    u64::from(at).min(size - WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    // The expected values come from MEGA's SDK tests (tests/unit/FileFingerprint_test.cpp, FileFingerprint_CRC_test.cpp), which give the CRC as four i32 read on a little-endian machine, or as the base64 of its 16 bytes.

    fn lanes(values: [i32; 4]) -> Crc {
        let mut crc = [0u8; 16];
        for (i, v) in values.into_iter().enumerate() {
            crc[i * 4..][..4].copy_from_slice(&v.to_le_bytes());
        }
        crc
    }

    fn file(dir: &TempDir, data: &[u8]) -> (File, u64) {
        let path = dir.write_file("f", data);
        (File::open(path).unwrap(), data.len() as u64)
    }

    fn crc_of(data: &[u8]) -> Crc {
        let dir = TempDir::new("mega_crc");
        let (mut f, size) = file(&dir, data);
        of(&mut f, size).unwrap()
    }

    fn counting(n: usize) -> Vec<u8> {
        (0..n).map(|i| i as u8).collect()
    }

    /// The SDK's fillDeterministic.
    fn xorshift(n: usize) -> Vec<u8> {
        let mut x: u32 = 0xA5A5_A5A5;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    /// MEGA's base64: URL alphabet without padding.
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn tiny_files_are_their_bytes() {
        assert_eq!(crc_of(&[3, 4, 5, 6]), lanes([100992003, 0, 0, 0]));
        assert_eq!(crc_of(&[]), [0; 16]);
    }

    #[test]
    fn small_files_get_one_crc_per_quarter() {
        assert_eq!(
            crc_of(&counting(100)),
            lanes([215253208, 661795201, 937191950, 562141813])
        );
    }

    #[test]
    fn larger_files_are_sampled() {
        assert_eq!(
            crc_of(&counting(20000)),
            lanes([-1424885571, 1204627086, 1194313128, -177560448])
        );
    }

    #[test]
    fn files_over_the_overflow_size_also_have_the_legacy_crc() {
        const MIB: usize = 1 << 20;
        let dir = TempDir::new("mega_crc_legacy");
        for (size, good, bug) in [
            (40 * MIB, "6iqpUy7DdAKx5NIRg31i_g", "6iqpUy7DdAKx5NIRGX1AAA"),
            (52 * MIB, "7SMVr_-v9_H7MDsN9yuVGA", "7SMVr_-v9_Gk00B4SWd30g"),
            (88 * MIB, "3hhTVPVhwzudmjN1odbO6w", "3hhTVIMatxXS_18ZkPyITg"),
        ] {
            let (mut f, len) = file(&dir, &xorshift(size));
            assert_eq!(base64(&of(&mut f, len).unwrap()), good, "{size}");
            assert_eq!(
                legacy(&mut f, len).unwrap().map(|crc| base64(&crc)),
                Some(bug.to_owned()),
                "{size}"
            );
        }
        let (mut f, len) = file(&dir, &counting(LEGACY_MIN_SIZE as usize - 1));
        assert_eq!(legacy(&mut f, len).unwrap(), None);
    }
}

//! Hostile files: truncation, bit flips, looping chains, absurd sizes, overlapping streams.
//! Every case must end in an error or a clean read, quickly, and never panic.

use std::sync::mpsc;
use std::time::Duration;

use soundcraft_cfb::{CompoundFile, ENDOFCHAIN, EntryKind, Error, Limits, NOSTREAM, Version, Writer};

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u32).wrapping_mul(31).wrapping_add(u32::from(seed)) as u8).collect()
}

/// A small file with a mini stream, a regular stream and a nested storage.
fn fixture(v: Version) -> Vec<u8> {
    let mut w = Writer::new(v);
    let a = w.storage(Writer::ROOT, "Alpha", [1; 16]).unwrap();
    w.stream(a, "tiny", pattern(10, 1)).unwrap();
    w.stream(a, "mini", pattern(3000, 2)).unwrap();
    w.stream(Writer::ROOT, "big", pattern(9000, 3)).unwrap();
    w.stream(Writer::ROOT, "empty", Vec::new()).unwrap();
    w.finish().unwrap()
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn sector_size(b: &[u8]) -> usize {
    1 << u16::from_le_bytes([b[30], b[31]])
}
/// Byte offset of directory entry `i` (the writer lays the directory out contiguously).
fn dir_entry(b: &[u8], i: usize) -> usize {
    (get32(b, 48) as usize + 1) * sector_size(b) + i * 128
}
/// Byte offset of the FAT slot of sector `s` (the first FAT sector covers the small fixtures).
fn fat_slot(b: &[u8], s: u32) -> usize {
    (get32(b, 76) as usize + 1) * sector_size(b) + s as usize * 4
}
/// Byte offset of the mini FAT slot of mini sector `s`.
fn minifat_slot(b: &[u8], s: u32) -> usize {
    (get32(b, 60) as usize + 1) * sector_size(b) + s as usize * 4
}

fn set_fat(b: &mut [u8], s: u32, v: u32) {
    let at = fat_slot(b, s);
    put32(b, at, v);
}
fn set_minifat(b: &mut [u8], s: u32, v: u32) {
    let at = minifat_slot(b, s);
    put32(b, at, v);
}

/// Open `bytes` and read every stream. Returns the first error, or the number of bytes read.
fn open_and_read_all(bytes: &[u8]) -> Result<usize, Error> {
    let cf = CompoundFile::open(bytes)?;
    let mut total = 0;
    let mut first_err = None;
    for i in (0..cf.entries.len()).filter(|&i| cf.entries[i].kind == EntryKind::Stream) {
        match cf.read(i) {
            Ok(d) => total += d.len(),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    first_err.map_or(Ok(total), Err)
}

/// Run `f` on its own thread; fail if it panics or runs for more than 60 s.
fn guarded<F: FnOnce() + Send + 'static>(f: F) {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => h.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            h.join().unwrap(); // re-raises the panic
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("hung"),
    }
}

#[test]
fn the_fixtures_are_valid() {
    for v in [Version::V3, Version::V4] {
        let b = fixture(v);
        assert_eq!(open_and_read_all(&b), Ok(10 + 3000 + 9000));
    }
}

#[test]
fn truncation_at_every_byte() {
    guarded(|| {
        for v in [Version::V3, Version::V4] {
            let b = fixture(v);
            let full = CompoundFile::open(&b).unwrap();
            let expect: Vec<(usize, Vec<u8>)> = (0..full.entries.len()).filter_map(|i| full.read(i).ok().map(|d| (i, d))).collect();
            for cut in 0..b.len() {
                if let Ok(cf) = CompoundFile::open(&b[..cut]) {
                    for (i, data) in &expect {
                        if let Ok(d) = cf.read(*i) {
                            assert_eq!(&d, data, "cut {cut}: a stream that reads back is complete");
                        }
                    }
                }
            }
            // below the header nothing is a compound file
            for cut in 0..512 {
                assert!(CompoundFile::open(&b[..cut]).is_err());
            }
        }
    });
}

#[test]
fn every_header_bit_flip() {
    guarded(|| {
        for v in [Version::V3, Version::V4] {
            let b = fixture(v);
            for at in 0..512 {
                for bit in 0..8 {
                    let mut g = b.clone();
                    g[at] ^= 1 << bit;
                    let _ = open_and_read_all(&g);
                }
            }
        }
    });
}

#[test]
fn every_header_field_set_to_extremes() {
    guarded(|| {
        let b = fixture(Version::V3);
        for at in (0..512).step_by(4) {
            for v in [0u32, 1, 0x7FFF_FFFF, 0xFFFF_FFF9, 0xFFFF_FFFA, 0xFFFF_FFFC, 0xFFFF_FFFD, 0xFFFF_FFFE, 0xFFFF_FFFF] {
                let mut g = b.clone();
                put32(&mut g, at, v);
                let _ = open_and_read_all(&g);
            }
        }
    });
}

#[test]
fn bit_flips_in_the_directory_and_fat() {
    guarded(|| {
        let b = fixture(Version::V3);
        let lo = dir_entry(&b, 0);
        for at in lo..b.len() {
            for bit in [0, 3, 7] {
                let mut g = b.clone();
                g[at] ^= 1 << bit;
                let _ = open_and_read_all(&g);
            }
        }
    });
}

#[test]
fn a_looping_stream_chain_is_an_error() {
    for v in [Version::V3, Version::V4] {
        let b = fixture(v);
        let cf = CompoundFile::open(&b).unwrap();
        let big = cf.find("big").unwrap();
        let start = cf.entries[big].start;
        // self loop, and a loop of two
        for next in [start, start + 1] {
            let mut g = b.clone();
            set_fat(&mut g, start, next);
            set_fat(&mut g, start + 1, start);
            let cf = CompoundFile::open(&g).unwrap();
            match cf.read(cf.find("big").unwrap()) {
                Err(Error::Invalid(m)) => assert!(m.contains("loops"), "{m}"),
                other => panic!("expected a loop error, got {other:?}"),
            }
        }
    }
}

#[test]
fn a_looping_mini_chain_is_an_error() {
    for v in [Version::V3, Version::V4] {
        let b = fixture(v);
        let cf = CompoundFile::open(&b).unwrap();
        let mini = cf.find("Alpha/mini").unwrap();
        let start = cf.entries[mini].start;
        let mut g = b.clone();
        set_minifat(&mut g, start, start);
        let cf = CompoundFile::open(&g).unwrap();
        match cf.read(cf.find("Alpha/mini").unwrap()) {
            Err(Error::Invalid(m)) => assert!(m.contains("loops"), "{m}"),
            other => panic!("expected a loop error, got {other:?}"),
        }
        // a long loop that does not touch the start: 0 -> 1 -> 2 -> 1
        let mut g = b.clone();
        set_minifat(&mut g, start, start + 1);
        set_minifat(&mut g, start + 1, start + 2);
        set_minifat(&mut g, start + 2, start + 1);
        let cf = CompoundFile::open(&g).unwrap();
        assert!(cf.read(cf.find("Alpha/mini").unwrap()).is_err());
    }
}

#[test]
fn a_looping_directory_chain_is_an_error_at_open() {
    for v in [Version::V3, Version::V4] {
        // enough entries to need several directory sectors in a 512-byte-sector file
        let mut w = Writer::new(v);
        for i in 0..40 {
            w.stream(Writer::ROOT, &format!("s{i}"), vec![1]).unwrap();
        }
        let b = w.finish().unwrap();
        let first = get32(&b, 48);
        let mut g = b.clone();
        // the first directory sector is followed by itself
        set_fat(&mut g, first, first);
        assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(m)) if m.contains("loops")));
    }
}

#[test]
fn a_looping_mini_fat_chain_is_an_error_at_open() {
    let mut w = Writer::new(Version::V3);
    // > 128 mini sectors make the mini FAT two sectors long
    for i in 0..60 {
        w.stream(Writer::ROOT, &format!("m{i}"), pattern(300, i as u8)).unwrap();
    }
    let b = w.finish().unwrap();
    let first = get32(&b, 60);
    let mut g = b.clone();
    set_fat(&mut g, first, first);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(_))));
}

#[test]
fn a_looping_difat_chain_is_an_error_at_open() {
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "big", pattern(7_500_000, 5)).unwrap();
    let b = w.finish().unwrap();
    let difat = get32(&b, 68);
    assert!(get32(&b, 72) >= 1, "the fixture needs a DIFAT sector");
    let mut g = b.clone();
    // the (only) DIFAT sector names itself as the next one
    let at = (difat as usize + 1) * 512 + 508;
    put32(&mut g, at, difat);
    // ... and the file claims more FAT sectors than the header and that sector can supply
    let n_fat = get32(&g, 44);
    put32(&mut g, 44, n_fat + 5);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(m)) if m.contains("DIFAT")));
}

#[test]
fn a_cyclic_directory_tree_is_an_error() {
    let b = fixture(Version::V3);
    // entry 1's left sibling is itself
    let mut g = b.clone();
    let at = dir_entry(&g, 1);
    put32(&mut g, at + 68, 1);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(_))));
    // the root's child is the root
    let mut g = b.clone();
    let at = dir_entry(&g, 0);
    put32(&mut g, at + 76, 0);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(_))));
    // a child index far outside the directory
    let mut g = b.clone();
    put32(&mut g, at + 76, 0x00FF_FFFF);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(_))));
}

#[test]
fn absurd_declared_sizes_are_errors_not_allocations() {
    for v in [Version::V3, Version::V4] {
        let b = fixture(v);
        let cf = CompoundFile::open(&b).unwrap();
        for path in ["big", "Alpha/mini", "Alpha/tiny"] {
            let id = cf.find(path).unwrap();
            for size in [b.len() as u64 + 1, 1 << 32, 1 << 40, u64::MAX / 2, u64::MAX] {
                let mut g = b.clone();
                let at = dir_entry(&g, id) + 120;
                put64(&mut g, at, size);
                let cf = CompoundFile::open(&g).unwrap();
                let r = cf.read(id);
                // (version 3 ignores the high 32 bits, so a size can become small and legal)
                if let Ok(d) = r {
                    assert!(d.len() as u64 <= b.len() as u64);
                }
            }
        }
        // the root's mini stream claims to be huge
        let mut g = b.clone();
        let at = dir_entry(&g, 0) + 120;
        put64(&mut g, at, u64::MAX);
        let _ = open_and_read_all(&g);
        // so many FAT sectors, DIFAT sectors, a directory in the stars
        for (at, val) in [(44, u32::MAX), (72, u32::MAX), (48, 0x00FF_FFFF), (60, 0x00FF_FFFF), (68, 0x00FF_FFFF), (44, 1 << 28)] {
            let mut g = b.clone();
            put32(&mut g, at, val);
            let _ = open_and_read_all(&g);
        }
    }
}

#[test]
fn a_directory_over_the_entry_limit_is_refused() {
    let mut w = Writer::new(Version::V3);
    for i in 0..200 {
        w.stream(Writer::ROOT, &format!("e{i}"), Vec::new()).unwrap();
    }
    let b = w.finish().unwrap();
    assert!(CompoundFile::open(&b).is_ok());
    let tight = Limits { max_entries: 100, ..Limits::for_len(b.len()) };
    assert!(matches!(CompoundFile::open_with(&b, tight), Err(Error::Invalid(m)) if m.contains("directory")), "limit on entries");
    // the default limit follows the file size
    assert_eq!(Limits::for_len(0).max_entries, 1);
    assert_eq!(Limits::for_len(128 * 1000).max_entries, 1001);
    assert_eq!(Limits::for_len(usize::MAX).max_entries, soundcraft_cfb::MAX_ENTRIES);
    assert!(Limits::for_len(usize::MAX).max_read_bytes <= soundcraft_cfb::MAX_READ_BYTES);
}

/// Many streams that claim the same sectors: the read budget stops the copying.
#[test]
fn overlapping_streams_run_into_the_read_budget() {
    guarded(|| {
        let mut w = Writer::new(Version::V3);
        w.stream(Writer::ROOT, "real", pattern(200_000, 9)).unwrap();
        let n = 400;
        for i in 0..n {
            w.stream(Writer::ROOT, &format!("alias{i:03}"), pattern(4096, 1)).unwrap();
        }
        let b = w.finish().unwrap();
        let cf = CompoundFile::open(&b).unwrap();
        let real = cf.entries[cf.find("real").unwrap()].clone();
        // every alias is made to claim the whole of "real"
        let mut g = b.clone();
        for i in 0..n {
            let id = cf.find(&format!("alias{i:03}")).unwrap();
            let at = dir_entry(&g, id);
            put32(&mut g, at + 116, real.start);
            put64(&mut g, at + 120, real.size);
        }
        let cf = CompoundFile::open(&g).unwrap();
        let (mut ok, mut refused, mut bytes) = (0, 0, 0u64);
        for i in (0..cf.entries.len()).filter(|&i| cf.entries[i].kind == EntryKind::Stream) {
            match cf.read(i) {
                Ok(d) => {
                    ok += 1;
                    bytes += d.len() as u64;
                }
                Err(Error::Invalid(m)) if m.contains("budget") => refused += 1,
                Err(_) => {}
            }
        }
        assert!(ok >= 1 && refused >= 1, "ok {ok}, refused {refused}");
        assert!(bytes <= cf.limits().max_read_bytes, "{bytes} > {}", cf.limits().max_read_bytes);
        assert_eq!(cf.read_budget_left(), cf.limits().max_read_bytes - bytes);
    });
}

#[test]
fn a_stream_cannot_share_a_sector_with_the_directory() {
    let mut w = Writer::new(Version::V3);
    w.stream(Writer::ROOT, "big", pattern(9000, 3)).unwrap();
    let b = w.finish().unwrap();
    // the stream's chain is pointed at the directory: reading it yields directory bytes, never a panic
    let mut g = b.clone();
    let id = {
        let cf = CompoundFile::open(&g).unwrap();
        cf.find("big").unwrap()
    };
    let at = dir_entry(&g, id);
    let dir = get32(&g, 48);
    put32(&mut g, at + 116, dir);
    let _ = open_and_read_all(&g);
    // the directory's chain pointed at the FAT sector itself: a claimed sector, so an error
    let mut g = b.clone();
    let fat = get32(&g, 76);
    put32(&mut g, 48, fat);
    assert!(CompoundFile::open(&g).is_err());
}

#[test]
fn two_header_slots_naming_one_fat_sector_is_an_error() {
    let b = fixture(Version::V3);
    let mut g = b.clone();
    put32(&mut g, 44, 2);
    let first = get32(&g, 76);
    put32(&mut g, 80, first);
    assert!(matches!(CompoundFile::open(&g), Err(Error::Invalid(m)) if m.contains("twice")));
}

#[test]
fn chains_ending_in_a_marker_are_errors() {
    let b = fixture(Version::V3);
    let cf = CompoundFile::open(&b).unwrap();
    let start = cf.entries[cf.find("big").unwrap()].start;
    for marker in [0xFFFF_FFFFu32, 0xFFFF_FFFC, 0xFFFF_FFFD, 0xFFFF_FFFB, ENDOFCHAIN, NOSTREAM] {
        let mut g = b.clone();
        set_fat(&mut g, start, marker);
        let cf = CompoundFile::open(&g).unwrap();
        // (ENDOFCHAIN ends the stream early: too short)
        assert!(cf.read(cf.find("big").unwrap()).is_err(), "marker {marker:#x}");
    }
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// Arbitrary bytes behind a real signature and header shape.
        #[test]
        fn random_bytes_after_a_signature(tail in proptest::collection::vec(any::<u8>(), 0..6000), v4 in any::<bool>()) {
            let mut b = fixture(if v4 { Version::V4 } else { Version::V3 });
            let keep = 8.min(b.len());
            b.truncate(keep);
            b.extend_from_slice(&tail);
            let _ = open_and_read_all(&b);
        }

        /// A valid file with a handful of bytes replaced.
        #[test]
        fn mutated_valid_file(edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..24), v4 in any::<bool>()) {
            let mut b = fixture(if v4 { Version::V4 } else { Version::V3 });
            for (ix, val) in edits {
                let at = ix.index(b.len());
                b[at] = val;
            }
            let _ = open_and_read_all(&b);
        }

        /// A valid file cut anywhere and followed by garbage.
        #[test]
        fn cut_and_garbage(cut in any::<prop::sample::Index>(), junk in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let mut b = fixture(Version::V3);
            let at = cut.index(b.len() + 1);
            b.truncate(at);
            b.extend_from_slice(&junk);
            let _ = open_and_read_all(&b);
        }
    }
}

/// The major version decides the sector size and how directory sizes are read; a header that
/// names an unknown version, or a sector size that does not go with its version, is refused.
#[test]
fn a_version_and_sector_shift_that_do_not_agree_are_refused() {
    for v in [Version::V3, Version::V4] {
        let good = fixture(v);
        assert!(CompoundFile::open(&good).is_ok());
        for version in [0u16, 1, 2, 5, 0x0100, 0xFFFF] {
            let mut b = good.clone();
            b[26..28].copy_from_slice(&version.to_le_bytes());
            assert!(CompoundFile::open(&b).is_err(), "version {version}");
        }
        for shift in [0u16, 7, 8, 10, 11, 13, 16] {
            let mut b = good.clone();
            b[30..32].copy_from_slice(&shift.to_le_bytes());
            assert!(CompoundFile::open(&b).is_err(), "{v:?} with shift {shift}");
        }
        // the other version's shift
        let mut b = good.clone();
        let other: u16 = if v == Version::V3 { 12 } else { 9 };
        b[30..32].copy_from_slice(&other.to_le_bytes());
        assert!(CompoundFile::open(&b).is_err());
    }
}

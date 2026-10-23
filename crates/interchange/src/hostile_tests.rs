//! Whole-file attacks on the importers: a file cut at every byte, with every bit of its header
//! flipped, with chains in its allocation tables that loop, and with sizes in it that are far
//! beyond the file. Each must give an error (or, for a change that leaves a valid file, a valid
//! import) without a panic, a hang or an allocation sized by the file's claims.

use std::time::{Duration, Instant};

use soundcraft_model::{ChannelFormat, Fade, FadeShape, MarkerKind, Session, TrackKind};
use soundcraft_time::SampleRate;

use crate::support::{add_source, clip_mut, demo_like, place};
use crate::{ExportOptions, Format, ImportOptions, MediaMode, export, import};

/// A session with one track, two clips, a crossfade, a fade and a marker: a file of a few KB.
fn tiny_session() -> Session {
    let mut s = Session::new("Tiny", SampleRate::HZ_48000);
    let src = add_source(&mut s, "A", 1, 48_000, 1);
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("T"));
    let a = place(&mut s, t, "a", src, 0, 0, 2_400);
    let b = place(&mut s, t, "b", src, 1_000, 2_000, 2_400);
    clip_mut(&mut s, a).fade_out = Fade { len: 400, shape: FadeShape::EqualPower, curvature: 0.0 };
    clip_mut(&mut s, b).fade_in = Fade { len: 400, shape: FadeShape::EqualPower, curvature: 0.0 };
    s.add_marker("M", MarkerKind::Marker, 100, 100);
    s
}

fn export_of(s: &Session, format: Format, small_sectors: bool, media: MediaMode) -> Vec<u8> {
    export(s, format, &ExportOptions { small_sectors, media, ..Default::default() }).expect("export").0
}

/// The files attacked: (label, format, bytes).
fn files() -> Vec<(&'static str, Format, Vec<u8>)> {
    let tiny = tiny_session();
    vec![
        ("AAF v3", Format::Aaf, export_of(&tiny, Format::Aaf, true, MediaMode::Link)),
        ("AAF v4", Format::Aaf, export_of(&tiny, Format::Aaf, false, MediaMode::Link)),
        ("AAF v3 embedded", Format::Aaf, export_of(&tiny, Format::Aaf, true, MediaMode::Embed)),
        ("OMF", Format::Omf, export_of(&tiny, Format::Omf, false, MediaMode::Link)),
        ("OMF embedded", Format::Omf, export_of(&tiny, Format::Omf, false, MediaMode::Embed)),
    ]
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn try_import(bytes: &[u8], format: Format) -> bool {
    import(bytes, format, &ImportOptions::default()).is_ok()
}

/// Run `f`, which must finish in a few seconds.
fn quickly<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    assert!(start.elapsed() < Duration::from_secs(20), "{what} took {:?}", start.elapsed());
    out
}

#[test]
fn the_files_attacked_are_valid() {
    for (name, format, bytes) in files() {
        assert!(try_import(&bytes, format), "{name}");
        assert_eq!(crate::detect(&bytes, None), Some(format), "{name}");
    }
}

/// Cut at every byte, every cut is an error: a container with a byte missing is not the file that
/// was written, and the readers say so rather than reading what is left.
#[test]
fn a_file_cut_at_every_byte_is_an_error() {
    for (name, format, bytes) in files() {
        quickly(name, || {
            let mut accepted = Vec::new();
            for cut in 0..bytes.len() {
                if try_import(&bytes[..cut], format) {
                    accepted.push(cut);
                }
            }
            assert!(accepted.is_empty(), "{name} ({} bytes) was accepted cut at {:?}", bytes.len(), &accepted[..accepted.len().min(10)]);
        });
    }
}

/// Every bit of the compound file header (and the first FAT sector's start, which the header
/// points at) and of the Bento label flipped. Flips that leave a file that still means the same
/// thing (reserved bytes, the transaction signature, the unused DIFAT slots) may import; the
/// fields that give the layout may not.
#[test]
fn every_bit_of_the_header_flipped() {
    for (name, format, bytes) in files() {
        let spans: Vec<(usize, usize)> = match format {
            Format::Aaf => vec![(0, 512)],
            Format::Omf => vec![(bytes.len() - 24, bytes.len())],
        };
        quickly(name, || {
            let mut ok = 0;
            let mut total = 0;
            for &(lo, hi) in &spans {
                for byte in lo..hi {
                    for bit in 0..8 {
                        let mut b = bytes.clone();
                        b[byte] ^= 1 << bit;
                        total += 1;
                        ok += usize::from(try_import(&b, format));
                    }
                }
            }
            eprintln!("{name}: {ok} of {total} header bit flips still import");
            if format == Format::Aaf {
                // the signature, the major version, byte order and sector shifts (24..26 is the
                // minor version, which readers ignore)
                for byte in (0..8).chain(26..34) {
                    for bit in 0..8 {
                        let mut b = bytes.clone();
                        b[byte] ^= 1 << bit;
                        assert!(!try_import(&b, format), "{name}: byte {byte} bit {bit}");
                    }
                }
            } else {
                // the magic
                for byte in 0..8 {
                    for bit in 0..8 {
                        let mut b = bytes.clone();
                        let at = bytes.len() - 24 + byte;
                        b[at] ^= 1 << bit;
                        assert!(!try_import(&b, format), "{name}: label byte {byte} bit {bit}");
                    }
                }
            }
        });
    }
}

/// Chains in the allocation tables that loop. Each is one entry changed to point at itself.
#[test]
fn cyclic_fat_chains_are_errors() {
    for (name, format, bytes) in files().into_iter().filter(|f| f.1 == Format::Aaf) {
        let ss = if bytes[26] == 4 { 4096 } else { 512 };
        let sector = |n: u32| (n as usize + 1) * ss;
        let fat0 = le32(&bytes, 76);
        let dir0 = le32(&bytes, 48);
        let fat_entry = |n: u32| sector(fat0) + n as usize * 4;

        // 1. the directory's own chain
        let mut b = bytes.clone();
        let next = le32(&b, fat_entry(dir0));
        assert!(next < 0xFFFF_FFFA, "{name}: the directory spans sectors");
        put32(&mut b, fat_entry(dir0), dir0);
        assert!(!try_import(&b, format), "{name}: directory chain looping on its first sector");
        // looping back to the first sector from the second
        let mut b = bytes.clone();
        put32(&mut b, fat_entry(next), dir0);
        assert!(!try_import(&b, format), "{name}: directory chain looping back");

        // 2. every chain of the file looped, one at a time
        let fat_len = ss / 4;
        let mut looped = 0;
        for n in 0..fat_len as u32 {
            let v = le32(&bytes, fat_entry(n));
            if v >= 0xFFFF_FFFA {
                continue;
            }
            let mut b = bytes.clone();
            put32(&mut b, fat_entry(n), n);
            quickly(name, || {
                let _ = try_import(&b, format);
            });
            looped += 1;
        }
        assert!(looped > 0, "{name}: no chains to loop");

        // 3. the mini FAT: every entry that is in use points at itself
        let mini0 = le32(&bytes, 60);
        if mini0 < 0xFFFF_FFFA {
            let mut b = bytes.clone();
            for n in 0..(ss / 4) as u32 {
                let at = sector(mini0) + n as usize * 4;
                if le32(&b, at) != 0xFFFF_FFFF {
                    put32(&mut b, at, n);
                }
            }
            assert!(!try_import(&b, format), "{name}: mini FAT chains looping");
        }

        // 4. a DIFAT chain whose sector names itself as the next. It takes a file with more
        // sectors than a header can list, so only the embedded file has room to claim 200 FAT
        // sectors: the last data sector is made the DIFAT sector and its link is itself.
        let n_sectors = bytes.len() / ss - 1;
        if n_sectors > 250 {
            let last = n_sectors as u32 - 1;
            let mut b = bytes.clone();
            put32(&mut b, 68, last); // first DIFAT sector
            put32(&mut b, 72, 1);
            put32(&mut b, 44, 200); // 200 FAT sectors: 109 in the header and the rest from the DIFAT
            put32(&mut b, sector(last) + ss - 4, last); // its link: itself
            let err = import(&b, format, &ImportOptions::default()).err().map(|e| e.to_string());
            assert!(err.is_some(), "{name}: DIFAT loop was accepted");
        }
    }
}

/// Sizes and counts far beyond the file, in the container header, the directory and the formats'
/// own structures: errors, quickly, with no allocation sized by them.
#[test]
fn huge_declared_sizes_are_errors() {
    for (name, format, bytes) in files() {
        quickly(name, || match format {
            Format::Aaf => {
                for (field, at) in [("FAT sectors", 44), ("first directory sector", 48), ("first mini FAT sector", 60)] {
                    for v in [0x7FFF_FFFF, 0xFFFF_FFF0, 0x0100_0000] {
                        let mut b = bytes.clone();
                        put32(&mut b, at, v);
                        assert!(!try_import(&b, format), "{name}: {field} = {v:#x}");
                    }
                }
                // the mini FAT and DIFAT sector counts are advisory (the chains are followed to
                // their ends), so a huge one may import, but it may not allocate by it or hang
                for at in [64, 72] {
                    for v in [0x7FFF_FFFF, 0xFFFF_FFF0] {
                        let mut b = bytes.clone();
                        put32(&mut b, at, v);
                        let _ = try_import(&b, format);
                    }
                }
                // a DIFAT chain that starts beyond the file when the header lists more FAT
                // sectors than it can hold
                let mut b = bytes.clone();
                put32(&mut b, 68, 0x7FFF_FFFF);
                put32(&mut b, 44, 110);
                assert!(!try_import(&b, format), "{name}: a DIFAT start beyond the file");
                // directory sectors (a v4 field)
                let mut b = bytes.clone();
                put32(&mut b, 40, 0x7FFF_FFFF);
                let _ = try_import(&b, format);
                // every directory entry's stream size set to 4 GiB - 1 and to 2^40 (v4)
                let ss = if bytes[26] == 4 { 4096 } else { 512 };
                let dir0 = le32(&bytes, 48) as usize;
                let dir = (dir0 + 1) * ss;
                for size in [0xFFFF_FFFFu64, 0x7FFF_FFFF, 1 << 40] {
                    for e in 0..(ss / 128) {
                        let mut b = bytes.clone();
                        let at = dir + e * 128 + 120;
                        b[at..at + 8].copy_from_slice(&size.to_le_bytes());
                        let _ = try_import(&b, format);
                    }
                }
                // a root entry (the mini stream) claiming more than the file
                let mut b = bytes.clone();
                b[dir + 120..dir + 128].copy_from_slice(&(1u64 << 40).to_le_bytes());
                assert!(!try_import(&b, format), "{name}: a root stream larger than the file");
            }
            Format::Omf => {
                let n = bytes.len();
                // the label's TOC length and offset (also covered in omf::tests), and every
                // 4-byte word of the TOC set to the largest value, one at a time
                let toc_at = u32::from_be_bytes(bytes[n - 8..n - 4].try_into().unwrap()) as usize;
                for at in toc_at..n - 24 {
                    for v in [u32::MAX, 0x7FFF_FFFF, 0x1000_0000] {
                        let mut b = bytes.clone();
                        if at + 4 <= n - 24 {
                            b[at..at + 4].copy_from_slice(&v.to_be_bytes());
                            let _ = try_import(&b, format);
                        }
                    }
                }
            }
        });
    }
}

/// Every byte of every file set to each of a few values, one at a time (about 6000 trials per
/// file): whatever the result, no panic, and no single import takes long. The demo-like session
/// has more of everything.
#[test]
fn single_byte_damage_never_panics() {
    let mut targets = files();
    targets.push(("AAF demo", Format::Aaf, export_of(&demo_like(), Format::Aaf, true, MediaMode::Link)));
    targets.push(("OMF demo", Format::Omf, export_of(&demo_like(), Format::Omf, false, MediaMode::Link)));
    for (name, format, bytes) in targets {
        let step = (bytes.len() * 3 / 6000).max(1);
        let mut slowest = Duration::ZERO;
        let mut ok = 0usize;
        let mut total = 0usize;
        for at in (0..bytes.len()).step_by(step) {
            for v in [0x00u8, 0xFF, 0x80] {
                if bytes[at] == v {
                    continue;
                }
                let mut b = bytes.clone();
                b[at] = v;
                total += 1;
                let start = Instant::now();
                ok += usize::from(try_import(&b, format));
                slowest = slowest.max(start.elapsed());
            }
        }
        eprintln!("{name}: {ok} of {total} single-byte changes still import; slowest {slowest:?}");
        assert!(slowest < Duration::from_secs(5), "{name}: one import took {slowest:?}");
    }
}

#[test]
fn empty_and_tiny_inputs_are_errors() {
    for format in Format::ALL {
        for n in [0usize, 1, 7, 8, 23, 24, 25, 511, 512, 513] {
            for fill in [0x00u8, 0xFF, 0x4C] {
                assert!(!try_import(&vec![fill; n], format), "{format:?} {n} x {fill:#x}");
            }
        }
    }
}

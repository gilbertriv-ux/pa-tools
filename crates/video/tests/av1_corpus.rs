//! Heavy AV1 checks over a directory of IVF streams (`PATOOLS_AV1_SEEDS=/path/to/dir`), for a human
//! or a nightly job. Both tests are `#[ignore]`d and skip without the directory.
//!
//! - `oracle_matrix`: every stream decodes to exactly what ffmpeg's libdav1d decodes (needs `ffmpeg`
//!   with libdav1d on the PATH). Streams whose pictures change size are skipped.
//! - `fuzz_corpus`: truncation, byte and bit flips and random mutation of every stream, with one and
//!   three threads, must never panic (`PATOOLS_FUZZ_ITERS`, default 400 rounds per stream).
//!
//! - `replay_saved`: decodes every `.frames` file in `PATOOLS_FUZZ_REPLAY` (what `PATOOLS_FUZZ_SAVE`
//!   wrote during a `fuzz_corpus` hunt) and fails if any of them panics.
//!
//! Make the streams outside the repository with any AV1 encoder, for example
//! `ffmpeg -f lavfi -i testsrc2=size=128x96 -frames:v 12 -c:v libaom-av1 -crf 32 -b:v 0 -f ivf a.ivf`.
#![cfg(feature = "av1-decode")]

use soundcraft_video::av1::{Decoder, Picture};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn seeds() -> Option<Vec<(String, Vec<Vec<u8>>)>> {
    let dir = PathBuf::from(std::env::var_os("PATOOLS_AV1_SEEDS")?);
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "ivf") {
            let frames = ivf_frames(&std::fs::read(&p).ok()?);
            if !frames.is_empty() {
                out.push((p.file_stem()?.to_string_lossy().into_owned(), frames));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else { return out };
    let mut pos = usize::from(u16::from_le_bytes([h[0], h[1]]));
    while let Some(hd) = data.get(pos..pos + 12) {
        let size = u32::from_le_bytes([hd[0], hd[1], hd[2], hd[3]]) as usize;
        pos += 12;
        let end = pos.saturating_add(size).min(data.len());
        out.push(data[pos..end].to_vec());
        pos = end;
    }
    out
}

fn pix_fmt(p: &Picture) -> String {
    let base = match (p.mono_chrome, p.subsampling_x, p.subsampling_y) {
        (true, ..) => "gray",
        (false, 1, 1) => "yuv420p",
        (false, 1, 0) => "yuv422p",
        _ => "yuv444p",
    };
    match p.bit_depth {
        8 => base.to_string(),
        d => format!("{base}{d}le"),
    }
}

fn raw(pics: &[Picture]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in pics {
        for plane in &p.planes {
            for &s in plane {
                if p.bit_depth == 8 {
                    out.push(s as u8);
                } else {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
    }
    out
}

#[test]
#[ignore = "needs PATOOLS_AV1_SEEDS and ffmpeg with libdav1d"]
fn oracle_matrix() {
    let Some(seeds) = seeds() else {
        eprintln!("skip: PATOOLS_AV1_SEEDS is not set");
        return;
    };
    let dir = PathBuf::from(std::env::var_os("PATOOLS_AV1_SEEDS").unwrap_or_default());
    let (mut exact, mut different, mut skipped) = (0, Vec::new(), 0);
    for (name, frames) in &seeds {
        let mut dec = Decoder::with_threads(1);
        let mut pics = Vec::new();
        let mut err = None;
        for (i, f) in frames.iter().enumerate() {
            match dec.decode_pts(f, i as i64) {
                Ok(p) => pics.extend(p),
                Err(e) => err = Some(e),
            }
        }
        pics.extend(dec.flush());
        if let Some(e) = err {
            different.push(format!("{name}: decode error {e}"));
            continue;
        }
        let Some(first) = pics.first() else {
            skipped += 1;
            continue;
        };
        if pics.iter().any(|p| (p.width, p.height, p.bit_depth) != (first.width, first.height, first.bit_depth)) {
            eprintln!("{name}: pictures change size, skipped");
            skipped += 1;
            continue;
        }
        let fmt = pix_fmt(first);
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-c:v", "libdav1d", "-i"])
            .arg(dir.join(format!("{name}.ivf")))
            .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", &fmt, "-"])
            .stdin(Stdio::null())
            .output();
        let Ok(out) = out else {
            eprintln!("skip: no ffmpeg");
            return;
        };
        let mine = raw(&pics);
        if out.stdout == mine {
            exact += 1;
            eprintln!("{name}: bit-exact ({} frames, {fmt}, {}x{})", pics.len(), first.width, first.height);
        } else {
            let at = out.stdout.iter().zip(&mine).position(|(a, b)| a != b);
            different.push(format!("{name}: {} bytes vs libdav1d {} ({fmt}), first difference at {at:?}", mine.len(), out.stdout.len()));
        }
    }
    eprintln!("oracle matrix: {exact} bit-exact, {} different, {skipped} skipped", different.len());
    assert!(different.is_empty(), "{different:#?}");
}

static PANICS: AtomicUsize = AtomicUsize::new(0);

fn count_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.location().is_some_and(|l| l.file().replace('\\', "/").contains("/av1/")) {
                PANICS.fetch_add(1, Ordering::SeqCst);
                eprintln!("AV1 PANIC: {info}");
            }
            prev(info);
        }));
    });
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn run(frames: &[Vec<u8>], label: &str, threads: &[usize]) -> bool {
    let before = PANICS.load(Ordering::SeqCst);
    let res = catch_unwind(AssertUnwindSafe(|| {
        for &t in threads {
            let mut dec = Decoder::with_threads(t);
            for (i, f) in frames.iter().enumerate() {
                let _ = dec.decode_pts(f, i as i64);
            }
            let _ = dec.flush_result();
        }
    }));
    let ok = res.is_ok() && PANICS.load(Ordering::SeqCst) == before;
    if !ok {
        // Keep the offending input so it can be turned into a regression test.
        if let Some(dir) = std::env::var_os("PATOOLS_FUZZ_SAVE") {
            let mut bytes = Vec::new();
            for f in frames {
                bytes.extend_from_slice(&(f.len() as u32).to_le_bytes());
                bytes.extend_from_slice(f);
            }
            let name: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            if let Ok(mut file) = std::fs::File::create(PathBuf::from(dir).join(format!("{name}.frames"))) {
                let _ = file.write_all(&bytes);
            }
        }
        eprintln!("PANIC in {label}");
    }
    ok
}

#[test]
#[ignore = "needs PATOOLS_AV1_SEEDS; a long hunt"]
fn fuzz_corpus() {
    let Some(seeds) = seeds() else {
        eprintln!("skip: PATOOLS_AV1_SEEDS is not set");
        return;
    };
    count_panics();
    let iters: usize = std::env::var("PATOOLS_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(400);
    let threads: Vec<usize> = if std::env::var_os("PATOOLS_FUZZ_THREADS").is_some() { vec![1, 3] } else { vec![1] };
    let mut failures = Vec::new();
    let only: Vec<String> = std::env::var("PATOOLS_FUZZ_ONLY").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    for (name, seed) in &seeds {
        if !only.is_empty() && !only.contains(name) {
            continue;
        }
        eprintln!("fuzzing {name}: {} frames, {} bytes", seed.len(), seed.iter().map(Vec::len).sum::<usize>());
        for fi in 0..seed.len().min(3) {
            let len = seed[fi].len();
            for n in (0..len).step_by((len / 40).max(1)) {
                let mut d = seed.clone();
                d[fi].truncate(n);
                if !run(&d, &format!("{name} trunc {fi} {n}"), &threads) {
                    failures.push(format!("{name} trunc {fi} {n}"));
                }
            }
        }
        // Every header byte of the first two temporal units (sequence header, frame headers), a few values each.
        for fi in 0..seed.len().min(2) {
            for p in 0..seed[fi].len().min(64) {
                for v in [0u8, 0xff, 0x80, 0x01, 0x7f] {
                    let mut d = seed.clone();
                    d[fi][p] = v;
                    if !run(&d, &format!("{name} byte {fi} {p} {v:#x}"), &threads) {
                        failures.push(format!("{name} byte {fi} {p} {v:#x}"));
                    }
                }
                let b = (p * 5 + fi) % 8;
                let mut d = seed.clone();
                d[fi][p] ^= 1 << b;
                if !run(&d, &format!("{name} flip {fi} {p} {b}"), &threads) {
                    failures.push(format!("{name} flip {fi} {p} {b}"));
                }
            }
        }
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ name.len() as u64 ^ (seed.len() as u64) << 20);
        for i in 0..iters {
            let mut d = seed.clone();
            for _ in 0..1 + rng.below(8) {
                let fi = rng.below(d.len());
                if d[fi].is_empty() {
                    continue;
                }
                let p = rng.below(d[fi].len());
                // Mostly the header region, sometimes anywhere.
                let p = if rng.below(3) == 0 { p } else { p.min(rng.below(120)) };
                match rng.below(3) {
                    0 => d[fi][p] ^= 1 << rng.below(8),
                    1 => d[fi][p] = rng.next() as u8,
                    _ => d[fi][p] = d[fi][p].wrapping_add(1),
                }
            }
            if !run(&d, &format!("{name} random {i}"), &threads) {
                failures.push(format!("{name} random {i}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} panics: {failures:#?}", failures.len());
}

#[test]
#[ignore = "needs PATOOLS_FUZZ_REPLAY, a directory of saved inputs"]
fn replay_saved() {
    let Some(dir) = std::env::var_os("PATOOLS_FUZZ_REPLAY") else {
        eprintln!("skip: PATOOLS_FUZZ_REPLAY is not set");
        return;
    };
    count_panics();
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    names.sort();
    let mut failures = Vec::new();
    for path in &names {
        let Ok(data) = std::fs::read(path) else { continue };
        let (mut frames, mut pos) = (Vec::new(), 0);
        while let Some(h) = data.get(pos..pos + 4) {
            let n = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
            pos += 4;
            let end = pos.saturating_add(n).min(data.len());
            frames.push(data[pos..end].to_vec());
            pos = end;
        }
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !run(&frames, &label, &[1, 3]) {
            failures.push(label);
        }
    }
    eprintln!("replayed {} inputs, {} panicked", names.len(), failures.len());
    assert!(failures.is_empty(), "{failures:#?}");
}

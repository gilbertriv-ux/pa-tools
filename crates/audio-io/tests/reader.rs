//! Disk reader regressions. Every generated fixture is exclusively created and retained.
use soundcraft_audio_io::reader::{BLOCK_FRAMES, CACHE_LIMIT_BYTES, FileAudio, global_cache_bytes};
use soundcraft_audio_io::{AudioBuffer, BitDepth, EncodeOptions, FileFormat, encode, wav_float_header};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn fixture(label: &str) -> (File, PathBuf) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1000 {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pa-disk-reader-{label}-{}-{stamp}-{n}.wav", std::process::id()));
        match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(file) => {
                eprintln!("CREATED {} (retained)", path.display());
                return (file, path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create fixture: {error}"),
        }
    }
    panic!("cannot reserve fixture")
}

#[test]
fn blocks_match_native_decode_across_formats_and_boundaries() {
    let samples = (0..10_000).map(|n| ((n as f32) * 0.01).sin() * 0.7).collect::<Vec<_>>();
    for depth in [BitDepth::Int16, BitDepth::Int24, BitDepth::Int32, BitDepth::Float32] {
        let (mut file, _) = fixture("formats");
        let input = AudioBuffer { sample_rate: 48_000, channels: vec![samples.clone(), samples.iter().map(|x| -*x).collect()] };
        let bytes = encode(&input, &EncodeOptions { format: FileFormat::Wav, bit_depth: depth, dither: false, bwf: None }).unwrap();
        let expected = soundcraft_audio_io::decode(&bytes, Some("wav")).unwrap().1;
        file.write_all(&bytes).unwrap();
        let reader = FileAudio::open(file).unwrap();
        assert_eq!(reader.info().frames, 10_000);
        for start in [0u64, 4095, 4096, 8192, 9999] {
            let block = reader.read_frames(start, BLOCK_FRAMES).unwrap();
            for ch in 0..2 {
                assert_eq!(block.channels[ch], expected.channels[ch][start as usize..(start as usize + BLOCK_FRAMES).min(10_000)]);
                assert_eq!(reader.sample(ch, start, false), Some(expected.channels[ch][start as usize]));
            }
        }
        assert_eq!(reader.sample(2, 0, false), None);
        assert_eq!(reader.sample(0, 10_000, false), None);
    }
}

#[test]
fn long_sparse_file_does_not_change_cache_size() {
    let (mut file, _) = fixture("two-hours");
    let frames = 48_000u64 * 60 * 60 * 2;
    let header = wav_float_header(2, 48_000, frames).unwrap();
    file.write_all(&header).unwrap();
    let end = header.len() as u64 + frames * 8;
    file.seek(SeekFrom::Start(end - 1)).unwrap();
    file.write_all(&[0]).unwrap();
    let reader = FileAudio::open(file).unwrap();
    assert_eq!(reader.info().frames, frames);
    assert_eq!(reader.cache_bytes(), 4 * BLOCK_FRAMES * 2 * 4);
    assert_eq!(reader.sample(0, frames - 1, false), Some(0.0));
    assert!(global_cache_bytes() <= CACHE_LIMIT_BYTES);
    assert!(reader.read_frames(0, BLOCK_FRAMES + 1).is_err());
}

#[test]
fn offline_reads_do_not_turn_background_publication_into_silence() {
    let (mut file, _) = fixture("concurrent");
    let frames = BLOCK_FRAMES * 12;
    file.write_all(&wav_float_header(1, 48_000, frames as u64).unwrap()).unwrap();
    for n in 0..frames {
        file.write_all(&(n as f32).to_le_bytes()).unwrap();
    }
    let source = FileAudio::open(file).unwrap();
    let other = std::sync::Arc::clone(&source);
    let worker = std::thread::spawn(move || {
        for n in 0..1000 {
            other.prefetch(((n * 7919) % frames) as u64);
        }
    });
    for n in 0..1000 {
        let at = ((n * 3571) % frames) as u64;
        assert_eq!(source.sample(0, at, false), Some(at as f32));
    }
    worker.join().unwrap();
    assert!(source.error().is_none());
}

#[test]
fn missing_data_is_rejected_without_allocating_declared_audio() {
    let (mut file, _) = fixture("truncated");
    file.write_all(&wav_float_header(2, 48_000, 48_000u64 * 60 * 60).unwrap()).unwrap();
    assert!(FileAudio::open(file).is_err());
}

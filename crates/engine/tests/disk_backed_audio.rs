//! Retained-file regressions for native disk source registration and save/reopen.
use serde_json::json;
use soundcraft_engine::{Engine, io};
use soundcraft_model::TrackId;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn directory(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for _ in 0..1000 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pa-disk-{label}-{}-{stamp}-{n}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("{e}"),
        }
    }
    panic!("cannot reserve retained fixture directory")
}
fn take(path: &Path, frames: u64) {
    let header = soundcraft_audio_io::wav_float_header(1, 48_000, frames).unwrap();
    let mut file = std::fs::File::create_new(path).unwrap();
    file.write_all(&header).unwrap();
    if frames > 0 {
        file.write_all(&0.25f32.to_le_bytes()).unwrap();
        if frames > 1 {
            file.seek(SeekFrom::Start(header.len() as u64 + (frames - 1) * 4)).unwrap();
            file.write_all(&(-0.5f32).to_le_bytes()).unwrap();
        }
    }
    file.sync_all().unwrap();
}
fn track(engine: &mut Engine) -> TrackId {
    engine.execute("track.new", &json!({"count":1,"format":"mono"})).unwrap();
    engine.session().tracks[0].id
}
#[test]
fn finishing_a_large_take_registers_file_storage_without_materializing_audio() {
    let dir = directory("large");
    let path = dir.join("take.wav");
    let frames = 48_000u64 * 60 * 60 * 2;
    take(&path, frames);
    let mut engine = Engine::default();
    let track = track(&mut engine);
    let clips = io::add_recording(&mut engine, 0, vec![(track, path.clone())], None);
    assert_eq!(clips.len(), 1);
    let metadata = &engine.session().sources[0];
    let audio = engine.session().pool.get(metadata.id).unwrap();
    assert!(audio.is_file_backed());
    assert!(audio.resident().is_none());
    assert_eq!(audio.frames() as u64, frames);
    assert!(audio.materialize(4 * 1024 * 1024).is_err());
    assert_eq!(audio.sample(0, 0, false), Some(0.25));
    assert_eq!(audio.sample(0, frames as usize - 1, false), Some(-0.5));
    assert!(path.is_file());
}
#[test]
fn native_import_save_and_reopen_remain_file_backed() {
    let dir = directory("roundtrip");
    let path = dir.join("take.wav");
    take(&path, 131_072);
    let mut engine = Engine::default();
    io::import_audio_file(&mut engine, path.to_str().unwrap(), None, 0).unwrap();
    let session_path = dir.join("Session").join("Session.scraft");
    io::save_session(&mut engine, session_path.to_str().unwrap()).unwrap();
    assert!(path.is_file(), "original take is retained");
    let current = &engine.session().sources[0];
    let current_audio = engine.session().pool.get(current.id).unwrap();
    assert_ne!(current_audio.backing_path(), Some(path.as_path()), "saving rebinds to its saved media");
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0; 4]).unwrap();
    assert_eq!(current_audio.read_range(0, 1).unwrap().channels[0], vec![0.25], "saved session no longer depends on original file");
    let mut reopened = Engine::default();
    assert!(io::open_session(&mut reopened, session_path.to_str().unwrap()).unwrap().is_empty());
    let source = &reopened.session().sources[0];
    let audio = reopened.session().pool.get(source.id).unwrap();
    assert!(audio.is_file_backed());
    assert!(audio.resident().is_none());
    assert_eq!(audio.frames(), 131_072);
    assert_eq!(audio.sample(0, 0, false), Some(0.25));
    assert_eq!(audio.sample(0, 131_071, false), Some(-0.5));
}
#[test]
fn changing_an_open_source_reports_failure_instead_of_exporting_silence() {
    let dir = directory("changed");
    let path = dir.join("take.wav");
    take(&path, 8192);
    let mut engine = Engine::default();
    io::import_audio_file(&mut engine, path.to_str().unwrap(), None, 0).unwrap();
    // Append new fixture data; no prior bytes are overwritten or removed.
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0; 4]).unwrap();
    let source = &engine.session().sources[0];
    let audio = engine.session().pool.get(source.id).unwrap();
    assert!(audio.read_range(0, 4096).is_err());
    assert!(soundcraft_mix::source_stream::first_error(engine.session()).is_some());
}

#[test]
fn repeated_save_retains_previous_document_and_stage() {
    let dir = directory("retained-save");
    let first = dir.join("Song.scraft");
    let mut engine = Engine::default();
    io::save_session(&mut engine, first.to_str().unwrap()).unwrap();
    let original = std::fs::read(&first).unwrap();
    let stage = first.with_extension("scraft.tmp");
    let staged = std::fs::read(&stage).unwrap();
    let result = engine.execute("session.save", &json!({})).unwrap();
    let next = PathBuf::from(result["path"].as_str().unwrap());
    assert_ne!(first, next);
    assert_eq!(std::fs::read(&first).unwrap(), original);
    assert_eq!(std::fs::read(&stage).unwrap(), staged);
    assert!(next.is_file());
    let mut reopened = Engine::default();
    io::open_session(&mut reopened, next.to_str().unwrap()).unwrap();
    assert_eq!(reopened.session().name, engine.session().name);
}

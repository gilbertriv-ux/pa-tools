//! Optional real-speech test of import, resampling, recognition and sample-based storage.
#![cfg(feature = "whisper")]

use serde_json::json;
use soundcraft_engine::Engine;
use soundcraft_model::TrackKind;

#[test]
fn speech_command_preserves_session_time_origin() {
    let (Some(models), Some(wav)) = (std::env::var_os("PA_SPEECH_MODELS_DIR"), std::env::var_os("PA_SPEECH_WAV_FIXTURE")) else {
        eprintln!("SKIPPED: set PA_SPEECH_MODELS_DIR and PA_SPEECH_WAV_FIXTURE for the real engine round-trip");
        return;
    };
    let (models, wav) = (std::path::PathBuf::from(models), std::path::PathBuf::from(wav));
    let mut engine = Engine::default();
    let start = engine.session().sample_rate.samples(2.0);
    engine.execute("file.import_audio", &json!({"path": wav, "at": start})).unwrap();
    let track = engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().id;
    let result = engine.execute("track.transcribe", &json!({"track": track.0, "models_dir": models, "diarize": false})).unwrap();
    assert_eq!(result["status"], "ok", "{result}");
    let transcript = engine.session().transcript(track).expect("transcript stored");
    assert!(!transcript.words.is_empty());
    let mut last = start;
    for word in &transcript.words {
        assert!(word.start >= last && word.end >= word.start);
        assert!(word.end <= engine.session().content_end());
        last = word.end;
    }
    assert!(transcript.text().to_lowercase().contains("transcription"), "{}", transcript.text());
    eprintln!("engine transcript: {}", transcript.text());
}

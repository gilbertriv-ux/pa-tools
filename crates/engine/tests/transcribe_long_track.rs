//! Timing check for transcribing a long track (ignored: it needs a long WAV, so it never runs in CI).
//!
//! ```text
//! python3 -I make_long_wav.py long.wav 60        # any long 48 kHz mono WAV will do
//! PA_LONG_WAV=long.wav cargo test -p soundcraft-engine --test transcribe_long_track -- --ignored --nocapture
//! ```
//!
//! It imports the WAV, puts a compressor (release 3 s) on its clip, and makes the job the Transcript
//! window makes. The point it shows: making the job returns at once however long the track is, and
//! the render, the conversion and the (instant, stand-in) recognition all happen on the worker.
//! The structural proof that making a job renders nothing is in `cmd/transcribe.rs`'s tests.

use serde_json::json;
use soundcraft_engine::Engine;
use soundcraft_engine::cmd::transcribe_job_with_transcriber;
use soundcraft_pa_speech::{Options, ProgressFn, SpeechError, Transcriber, Transcript};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A recogniser that answers at once, remembering how much audio it was given.
struct Instant16k(Mutex<usize>);

impl Transcriber for Instant16k {
    fn id(&self) -> String {
        "instant".into()
    }
    fn transcribe(&self, audio: &[f32], _: &Options, _: ProgressFn) -> Result<Transcript, SpeechError> {
        if let Ok(mut n) = self.0.lock() {
            *n = audio.len();
        }
        Ok(Transcript { words: Vec::new(), language: "en".into(), source: self.id() })
    }
}

#[test]
#[ignore = "needs PA_LONG_WAV, a long WAV file"]
fn making_the_job_for_a_long_track_returns_at_once_and_the_worker_does_the_rest() {
    let Ok(wav) = std::env::var("PA_LONG_WAV") else {
        eprintln!("SKIPPED: set PA_LONG_WAV to a long WAV file");
        return;
    };
    let mut e = Engine::default();
    let t = Instant::now();
    let imported = e.execute("file.import_audio", &json!({"path": wav})).unwrap();
    let (track, clip) = (imported["imported"][0]["track"].clone(), imported["imported"][0]["clip"].clone());
    e.execute(
        "clip.effects_set",
        &json!({"clip": clip, "params": {"comp.threshold": -30.0, "comp.ratio": 4.0, "comp.attack": 5.0, "comp.release": 3000.0, "comp.lookahead": 5.0}}),
    )
    .unwrap();
    eprintln!("import + clip compressor: {:?}", t.elapsed());

    let recogniser = Arc::new(Instant16k(Mutex::new(0)));
    let started = Instant::now();
    let job = transcribe_job_with_transcriber(&e, &json!({"track": track}), recogniser.clone()).unwrap();
    let made_in = started.elapsed();
    eprintln!("transcribe_job returned after {made_in:?} for {:.1} minutes of audio", job.duration_secs() / 60.0);

    let worker = std::thread::spawn(move || {
        let began = Instant::now();
        let mut stages: Vec<(String, std::time::Duration)> = Vec::new();
        let outcome = job.run(&mut |_, stage| {
            if stages.last().is_none_or(|(s, _)| s != stage) {
                stages.push((stage.to_string(), began.elapsed()));
            }
            true
        });
        (outcome.map(|o| o.result["status"].clone()), stages, began.elapsed())
    });
    let (status, stages, total) = worker.join().unwrap();
    for (stage, at) in &stages {
        eprintln!("  worker: `{stage}` began {at:?} after it started");
    }
    eprintln!("worker: the whole job took {total:?}; status {:?}", status.as_ref().unwrap());
    let heard = recogniser.0.lock().map(|n| *n).unwrap_or(0);
    eprintln!("the recogniser was given {heard} samples at 16 kHz");
    assert_eq!(status.unwrap(), "ok");
    assert!(heard > 0);
    assert!(made_in < total / 20, "making the job ({made_in:?}) is not a small fraction of the work ({total:?})");
}

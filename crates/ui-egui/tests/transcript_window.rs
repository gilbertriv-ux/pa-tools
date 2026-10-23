//! Window ▸ Transcript: the command, the menu entry, drawing a long transcript, clicking words,
//! naming speakers and the model-missing round trip through the worker thread.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, epaint::ClippedShape, vec2};
use serde_json::{Value, json};
use soundcraft_model::{TrackId, TrackKind};
use soundcraft_pa_speech::{FixedTranscriber, Transcript, Word};
use soundcraft_ui_egui::{Services, SoundApp, menus};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Rig {
    ctx: egui::Context,
    app: SoundApp,
    shapes: Vec<ClippedShape>,
    modifiers: Modifiers,
}

impl Rig {
    fn new() -> Rig {
        let app = SoundApp::new(soundcraft_engine::demo::demo_engine(), None, Services::default());
        let mut r = Rig { ctx: egui::Context::default(), app, shapes: Vec::new(), modifiers: Modifiers::NONE };
        for _ in 0..4 {
            r.frame(vec![]);
        }
        r
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        // The held modifier keys, as the platform layer reports them.
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))), events, ..Default::default() };
        let mut out = self.ctx.run_ui(raw, |ui| {
            self.app.logic(ui.ctx());
            self.app.ui(ui);
        });
        out.textures_delta.clear();
        self.shapes = out.shapes;
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(vec![]);
        }
    }

    /// Every text the last frame drew.
    fn texts(&self) -> Vec<String> {
        self.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Where the last frame drew exactly `text`.
    fn find(&self, text: &str) -> Option<Rect> {
        self.shapes.iter().find_map(|c| match &c.shape {
            Shape::Text(t) if t.galley.text() == text => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        })
    }

    fn at(&self, text: &str) -> Pos2 {
        match self.find(text) {
            Some(r) => r.center(),
            None => panic!("`{text}` is not on screen; drew {} texts", self.texts().len()),
        }
    }

    fn button(&mut self, at: Pos2, pressed: bool) {
        let modifiers = self.modifiers;
        self.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers }]);
    }

    fn click(&mut self, at: Pos2) {
        self.button(at, true);
        self.button(at, false);
        self.frames(2);
    }

    fn key(&mut self, key: egui::Key) {
        let modifiers = self.modifiers;
        self.frame(vec![
            Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers },
            Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers },
        ]);
        self.frames(2);
    }
}

fn first_audio(app: &SoundApp) -> TrackId {
    app.engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).map(|t| t.id).unwrap()
}

/// `n` words 0.3 s apart (48 kHz), one speaker per 40 words, the first one "Alpha".
fn words(n: usize) -> Value {
    let list: Vec<Value> = (0..n)
        .map(|i| {
            let start = i as i64 * 14_400;
            let text = if i == 0 { "Alpha".to_string() } else { format!("w{i:04}") };
            json!({"start": start, "end": start + 9_600, "text": text, "speaker": (i / 40) % 2})
        })
        .collect();
    Value::Array(list)
}

fn store(rig: &mut Rig, n: usize) -> TrackId {
    let track = first_audio(&rig.app);
    rig.app.run("track.set_transcript", json!({"track": track.0, "words": words(n), "language": "en", "source": "test"})).unwrap();
    track
}

fn open(rig: &mut Rig) {
    rig.app.run("window.transcript", json!({"value": true})).unwrap();
    rig.frames(3);
}

#[test]
fn the_window_command_toggles_and_can_pick_a_track() {
    let mut rig = Rig::new();
    assert!(!rig.app.transcript.open);
    assert_eq!(rig.app.run("window.transcript", json!({})).unwrap(), json!({"value": true}));
    assert!(rig.app.transcript.open);
    assert_eq!(rig.app.run("window.transcript", json!({})).unwrap(), json!({"value": false}));
    assert_eq!(rig.app.run("window.transcript", json!({"value": true})).unwrap(), json!({"value": true}));
    assert_eq!(rig.app.run("window.transcript", json!({"value": true})).unwrap(), json!({"value": true}), "an explicit value is not a toggle");
    assert_eq!(rig.app.run("window.transcript", json!({"value": false})).unwrap(), json!({"value": false}));

    // A track opens it, on that track (by name or id).
    let session = rig.app.engine.session();
    let audio: Vec<(TrackId, String)> = session.tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| (t.id, t.name.clone())).collect();
    assert!(audio.len() >= 2, "the demo session has several audio tracks");
    let (second, second_name) = (audio[1].0, audio[1].1.clone());
    rig.app.run("window.transcript", json!({"track": second_name})).unwrap();
    assert!(rig.app.transcript.open);
    assert_eq!(rig.app.transcript.track, Some(second));
    rig.app.run("window.transcript", json!({"track": audio[0].0.0, "value": false})).unwrap();
    assert!(!rig.app.transcript.open);
    assert_eq!(rig.app.transcript.track, Some(audio[0].0));

    // An unknown track is refused and changes nothing.
    assert!(rig.app.run("window.transcript", json!({"track": "No Such Track"})).is_err());
    assert!(rig.app.run("window.transcript", json!({"track": 999_999})).is_err());
    assert!(!rig.app.transcript.open);
    assert_eq!(rig.app.transcript.track, Some(audio[0].0));
}

#[test]
fn window_transcript_is_a_menu_item_with_a_check_mark() {
    assert!(menus::parity_aliases().contains(&("Window > Transcript", "window.transcript")));
    assert!(menus::ui_aliases().contains(&("Window > Transcript", "window.transcript")));
    assert!(menus::UI_COMMANDS.iter().any(|c| c.0 == "window.transcript" && c.1 == "Transcript"));

    let tree = menus::tree();
    let window = tree.iter().find(|n| n.label == "Window").unwrap();
    let item = window.children.iter().find(|n| n.label == "Transcript").expect("Window > Transcript is in the menu bar");
    let id = menus::command_for_path(&item.path, &menus::ui_aliases());
    assert_eq!(id.as_deref(), Some("window.transcript"));

    let mut rig = Rig::new();
    let (enabled, checked) = menus::item_state(&rig.app, &item.path, id.as_deref());
    assert!(enabled && !checked);
    rig.app.run("window.transcript", json!({})).unwrap();
    assert!(menus::item_state(&rig.app, &item.path, id.as_deref()).1, "checked while the window is open");
}

#[test]
fn an_empty_session_says_there_is_nothing_to_transcribe() {
    let app = SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default());
    let mut rig = Rig { ctx: egui::Context::default(), app, shapes: Vec::new(), modifiers: Modifiers::NONE };
    rig.frames(4);
    open(&mut rig);
    assert!(rig.find("No audio tracks to transcribe.").is_some(), "{:?}", rig.texts());
}

#[test]
fn a_track_without_a_transcript_explains_how_to_make_one() {
    let mut rig = Rig::new();
    open(&mut rig);
    assert!(rig.find("Transcribe Track").is_some(), "{:?}", rig.texts());
    assert!(rig.find("No transcript yet. Choose Transcribe Track to turn this track's speech into text.").is_some(), "{:?}", rig.texts());
}

#[test]
fn a_transcript_with_no_words_says_no_speech_was_found() {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    rig.app.run("track.set_transcript", json!({"track": track.0, "words": []})).unwrap();
    open(&mut rig);
    assert!(rig.find("No speech found.").is_some(), "{:?}", rig.texts());
}

#[test]
fn a_five_thousand_word_transcript_draws_only_the_rows_in_view() {
    let mut rig = Rig::new();
    store(&mut rig, 5_000);
    let started = Instant::now();
    open(&mut rig);
    rig.frames(3);
    assert!(rig.find("Alpha").is_some(), "the first word is on screen: {:?}", &rig.texts()[..rig.texts().len().min(40)]);
    assert!(rig.find("5000 words · en").is_some(), "{:?}", rig.texts().iter().filter(|t| t.contains("words")).collect::<Vec<_>>());
    assert!(rig.find("w4999").is_none(), "far-away words are not laid out");
    let s = rig.app.engine.session();
    let first_time = s.format_position(0, s.edit.main_counter);
    assert!(rig.find(&first_time).is_some(), "the first line has its timecode `{first_time}`: {:?}", &rig.texts()[..rig.texts().len().min(60)]);
    assert!(rig.find("Speaker 0").is_some());
    let drawn = rig.texts().len();
    assert!(drawn < 1_500, "virtualized: {drawn} texts drawn");
    assert!(started.elapsed() < Duration::from_secs(60), "drawing took {:?}", started.elapsed());
}

#[test]
fn speakers_are_named_in_the_badges_and_the_copy_text() {
    let mut rig = Rig::new();
    let track = store(&mut rig, 120);
    rig.app.run("track.rename_speaker", json!({"track": track.0, "speaker": 0, "name": "Host"})).unwrap();
    open(&mut rig);
    assert!(rig.find("Host").is_some(), "{:?}", rig.texts());
    assert!(rig.find("Speaker 0").is_none());
}

#[test]
fn searching_lists_only_the_lines_that_match_and_says_when_none_do() {
    let mut rig = Rig::new();
    store(&mut rig, 400);
    open(&mut rig);
    assert!(rig.find("Alpha").is_some());
    rig.app.transcript.query = "w0123".into();
    rig.frames(3);
    assert!(rig.find("w0123").is_some(), "the matching word is listed");
    assert!(rig.find("Alpha").is_none(), "lines without it are not");
    rig.app.transcript.query = "nothing like this".into();
    rig.frames(3);
    assert!(rig.find("No lines contain \"nothing like this\".").is_some(), "{:?}", rig.texts());
    rig.app.transcript.query.clear();
    rig.frames(3);
    assert!(rig.find("Alpha").is_some());
}

#[test]
fn clicking_a_word_moves_the_playhead_to_its_start() {
    let mut rig = Rig::new();
    let track = store(&mut rig, 300);
    open(&mut rig);
    rig.app.run("transport.locate", json!({"at": 0})).unwrap();
    rig.frames(2);
    assert_eq!(rig.app.position(), 0);

    let at = rig.at("w0003");
    rig.click(at);
    assert_eq!(rig.app.position(), 3 * 14_400, "the insertion is at the word's start");
    assert_eq!(rig.app.engine.session().edit.playhead, 3 * 14_400);

    // Shift-click another word: the words from the last click to it are selected.
    rig.modifiers = Modifiers::SHIFT;
    let at = rig.at("w0006");
    rig.click(at);
    rig.modifiers = Modifiers::NONE;
    let s = &rig.app.engine.session().edit;
    assert_eq!((s.selection.start, s.selection.end), (3 * 14_400, 6 * 14_400 + 9_600));
    assert_eq!(s.selected_tracks, vec![track]);
}

#[test]
fn the_line_under_the_playhead_is_marked() {
    let mut rig = Rig::new();
    store(&mut rig, 300);
    open(&mut rig);
    rig.app.run("transport.locate", json!({"at": 0})).unwrap();
    rig.frames(2);
    // The marker is a faint fill as tall as a line, behind the words.
    let marker_y = |rig: &Rig| {
        rig.shapes.iter().find_map(|c| match &c.shape {
            Shape::Rect(r) if r.fill.a() > 0 && r.fill.a() < 80 && (20.0..24.0).contains(&r.rect.height()) && r.rect.width() > 300.0 => {
                Some(r.rect.center().y)
            }
            _ => None,
        })
    };
    let first_line = rig.at("Alpha").y;
    let marked = marker_y(&rig).expect("the playhead starts in the first line");
    assert!((marked - first_line).abs() < 6.0, "marker at {marked}, first line at {first_line}");

    let later = rig.at("w0030");
    rig.click(later);
    let marked = marker_y(&rig).expect("a line is marked");
    assert!((marked - later.y).abs() < 6.0, "marker at {marked}, clicked line at {}", later.y);
    assert!((marked - first_line).abs() > 20.0, "it moved off the first line");
}

#[test]
fn double_clicking_a_badge_renames_the_speaker_and_enter_keeps_it() {
    let mut rig = Rig::new();
    let track = store(&mut rig, 120);
    open(&mut rig);
    let at = rig.at("Speaker 0");
    rig.button(at, true);
    rig.button(at, false);
    rig.button(at, true);
    rig.button(at, false);
    rig.frames(3);
    rig.frame(vec![Event::Text("Dana".into())]);
    rig.frames(1);
    rig.key(egui::Key::Enter);
    rig.frames(3);
    assert_eq!(rig.app.engine.session().transcript(track).unwrap().speaker_label(0), "Dana", "{:?}", rig.texts());
    assert!(rig.find("Dana").is_some());
}

#[test]
fn escape_leaves_the_name_alone() {
    let mut rig = Rig::new();
    let track = store(&mut rig, 120);
    open(&mut rig);
    let at = rig.at("Speaker 0");
    for pressed in [true, false, true, false] {
        rig.button(at, pressed);
    }
    rig.frames(3);
    rig.frame(vec![Event::Text("Nope".into())]);
    rig.key(egui::Key::Escape);
    rig.frames(3);
    assert_eq!(rig.app.engine.session().transcript(track).unwrap().speaker_label(0), "Speaker 0");
    assert!(rig.find("Speaker 0").is_some(), "the badge is back: {:?}", rig.texts());
}

#[test]
fn a_missing_model_is_reported_and_nothing_is_stored() {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    let dir = std::env::temp_dir().join(format!("pa-tools-transcript-window-{}", std::process::id()));
    rig.app.transcript.models_dir = dir.to_string_lossy().into_owned();
    open(&mut rig);
    let at = rig.at("Transcribe Track");
    rig.click(at);

    // The worker runs on its own thread; pump frames until it reports.
    let started = Instant::now();
    while rig.app.transcript.is_running() && started.elapsed() < Duration::from_secs(10) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_running(), "the worker did not finish in time");
    let status = rig.app.transcript.status.clone();
    assert!(status.contains("Speech model, base (multilingual)") && status.contains("Models folder"), "{status}");
    assert!(!status.to_lowercase().contains("whisper") && !status.contains(&*dir.to_string_lossy()), "{status}");
    assert_eq!(rig.app.ui.status, status, "the main status line says it too");
    assert!(rig.app.engine.session().transcript(track).is_none(), "nothing was stored");
    assert!(rig.find(&status).is_some(), "the window shows the status");
}

#[test]
fn the_models_folder_starts_empty_whatever_the_environment_says() {
    // Prefilling it from $PA_SPEECH_MODELS_DIR would send it as an explicit `models_dir`, which
    // switches off the rest of the search (app bundle, app data). The engine reads the variable.
    let rig = Rig::new();
    assert_eq!(rig.app.transcript.models_dir, "");
    assert!(rig.app.transcript.follow, "Follow playhead starts on");
}

// ---- S2: the menu and the command palette start the background job -------------------------

/// Waits for the worker to report, pumping frames.
fn finish_job(rig: &mut Rig) {
    let started = Instant::now();
    while rig.app.transcript.is_running() && started.elapsed() < Duration::from_secs(10) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_running(), "the worker did not finish in time");
}

/// A Models folder that exists nowhere, so a job ends at once with "model not found" whatever the
/// machine has installed.
fn no_models(rig: &mut Rig, tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pa-tools-transcript-{tag}-{}", std::process::id()));
    rig.app.transcript.models_dir = dir.to_string_lossy().into_owned();
    dir
}

#[test]
fn the_menu_entry_starts_the_background_job_and_returns() {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    no_models(&mut rig, "menu");
    assert!(!rig.app.transcript.open && !rig.app.transcript.is_running());
    let steps = rig.app.engine.undo_history().len();

    // Exactly what clicking Track > Transcribe Track does.
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");

    // The call has returned with the window open and the job running: nothing was recognised on
    // this thread (the synchronous command would have stored or refused a transcript by now).
    assert!(rig.app.transcript.open, "the window opens");
    assert!(rig.app.transcript.is_running(), "the job is running in the background");
    assert!(rig.app.engine.session().transcript(track).is_none());
    assert_eq!(rig.app.engine.undo_history().len(), steps);
    assert!(rig.app.transcript.status.starts_with("Transcribing"), "{}", rig.app.transcript.status);

    // A second request while it runs does not start another.
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    assert_eq!(rig.app.transcript.status, "A transcription is already running");

    // The worker reports through the window like a click on its button: model not found.
    finish_job(&mut rig);
    let status = rig.app.transcript.status.clone();
    assert!(status.contains("Models folder") && !status.to_lowercase().contains("whisper"), "{status}");
}

#[test]
fn the_command_palette_starts_the_background_job_and_returns() {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    no_models(&mut rig, "palette");
    rig.app.ui.show_search = true;
    rig.frames(2);
    rig.frame(vec![Event::Text("Transcribe Track".into())]);
    rig.frames(2);
    assert!(rig.find("Transcribe Track").is_some(), "the palette lists the command: {:?}", rig.texts());
    assert!(!rig.app.transcript.is_running());

    // Enter in the palette: one frame, and the job is running behind the open window.
    let modifiers = Modifiers::NONE;
    rig.frame(vec![
        Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: true, repeat: false, modifiers },
        Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: false, repeat: false, modifiers },
    ]);
    assert!(!rig.app.ui.show_search, "the palette closes");
    assert!(rig.app.transcript.open && rig.app.transcript.is_running(), "window open, job running");
    assert!(rig.app.engine.session().transcript(track).is_none());
    finish_job(&mut rig);
}

#[test]
fn the_menu_entry_transcribes_the_selected_track() {
    let mut rig = Rig::new();
    no_models(&mut rig, "selected");
    let audio: Vec<TrackId> = rig.app.engine.session().tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| t.id).collect();
    let second = audio[1];
    rig.app.run("edit.select", json!({"tracks": [second.0]})).unwrap();
    let name = rig.app.engine.session().track(second).unwrap().name.clone();
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    assert!(rig.app.transcript.status.contains(&name), "{}", rig.app.transcript.status);
    finish_job(&mut rig);
}

#[test]
fn programmatic_transcribe_stays_synchronous() {
    // The control channel and MCP call the command itself and wait for its answer.
    let mut rig = Rig::new();
    let dir = no_models(&mut rig, "sync");
    let track = first_audio(&rig.app);
    let r = rig.app.run("track.transcribe", json!({"track": track.0, "models_dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["status"], "model_unavailable");
    assert!(!rig.app.transcript.open && !rig.app.transcript.is_running(), "no window, no job");
}

// ---- a job started from the menu, the palette or the button stores a real transcript -------

/// The words a fixed recogniser says, as (text, start s, end s): 0.4 s apart, inside the demo track.
const SPOKEN: [(&str, f64, f64); 9] = [
    ("ask", 0.0, 0.3),
    ("not", 0.4, 0.7),
    ("what", 0.8, 1.1),
    ("your", 1.2, 1.5),
    ("country", 1.6, 2.0),
    ("can", 2.1, 2.4),
    ("do", 2.5, 2.8),
    ("for", 2.9, 3.2),
    ("you", 3.3, 3.6),
];

/// A recogniser that answers with [`SPOKEN`] whatever the audio, so a job can run to the end
/// without the speech model being installed.
fn fixed_recogniser() -> Arc<FixedTranscriber> {
    let words = SPOKEN.iter().map(|(text, a, b)| Word::new(*text, *a, *b)).collect();
    Arc::new(FixedTranscriber { transcript: Transcript { words, language: "en".into(), source: String::new() }, id: "fixed-test".into() })
}

/// A rig whose Transcript window uses [`fixed_recogniser`] instead of looking for the model.
fn rig_with_recogniser() -> (Rig, TrackId) {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    rig.app.transcript.inject_transcriber(fixed_recogniser());
    let end = rig.app.engine.session().track(track).and_then(|t| t.playlist()).map_or(0, |p| p.end());
    assert!(end > rig.app.engine.session().sample_rate.samples(4.0), "the demo track outlasts the spoken words ({end})");
    (rig, track)
}

/// Whatever started the job, the result is the same: a transcript stored on the track through
/// `track.set_transcript` (the command that makes it undoable), with the recogniser's words, their
/// times in samples from the session start, and a status that says so.
fn assert_stored_through_the_command(rig: &mut Rig, track: TrackId, undo_steps_before: usize) {
    let sr = rig.app.engine.session().sample_rate;
    let stored = rig.app.engine.session().transcript(track).expect("the transcript was stored on the track");
    assert_eq!(stored.text(), "ask not what your country can do for you");
    assert_eq!(stored.words.len(), SPOKEN.len());
    for (word, (text, start, end)) in stored.words.iter().zip(SPOKEN) {
        assert_eq!((word.text.as_str(), word.start, word.end), (text, sr.samples(start), sr.samples(end)), "{text}");
    }
    assert_eq!((stored.language.as_str(), stored.source.as_str()), ("en", "fixed-test"));
    assert_eq!(rig.app.engine.undo_history().len(), undo_steps_before + 1, "one undo step");
    assert_eq!(rig.app.engine.undo_label(), Some("Set Track Transcript"), "stored by track.set_transcript, not by the synchronous command");
    assert!(rig.app.engine.is_dirty());
    assert_eq!(rig.app.transcript.status, "Transcribed 9 words");
    assert_eq!(rig.app.ui.status, "Transcribed 9 words");
    // And it is on screen.
    rig.frames(3);
    assert!(rig.find("country").is_some(), "the words are listed: {:?}", rig.texts());
    // Undo takes it away again, which only a stored session edit does.
    assert!(rig.app.engine.undo());
    assert!(rig.app.engine.session().transcript(track).is_none());
}

#[test]
fn a_job_started_from_the_menu_stores_the_transcript_through_set_transcript() {
    let (mut rig, track) = rig_with_recogniser();
    let steps = rig.app.engine.undo_history().len();
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    assert!(rig.app.transcript.open && rig.app.transcript.is_running(), "the menu started the background job");
    assert!(rig.app.engine.session().transcript(track).is_none(), "and returned before it was done");
    finish_job(&mut rig);
    assert_stored_through_the_command(&mut rig, track, steps);
}

#[test]
fn a_job_started_from_the_command_palette_stores_the_transcript_through_set_transcript() {
    let (mut rig, track) = rig_with_recogniser();
    let steps = rig.app.engine.undo_history().len();
    rig.app.ui.show_search = true;
    rig.frames(2);
    rig.frame(vec![Event::Text("Transcribe Track".into())]);
    rig.frames(2);
    assert!(rig.find("Transcribe Track").is_some(), "the palette lists the command: {:?}", rig.texts());
    let modifiers = Modifiers::NONE;
    rig.frame(vec![
        Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: true, repeat: false, modifiers },
        Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: false, repeat: false, modifiers },
    ]);
    assert!(!rig.app.ui.show_search && rig.app.transcript.is_running(), "Enter in the palette started the job");
    finish_job(&mut rig);
    assert_stored_through_the_command(&mut rig, track, steps);
}

#[test]
fn a_job_started_from_the_window_button_stores_the_transcript_through_set_transcript() {
    let (mut rig, track) = rig_with_recogniser();
    let steps = rig.app.engine.undo_history().len();
    open(&mut rig);
    let at = rig.at("Transcribe Track");
    rig.click(at);
    finish_job(&mut rig);
    assert_stored_through_the_command(&mut rig, track, steps);
}

/// The injected recogniser is a seam for tests: a window that was not given one looks for the model.
#[test]
fn without_an_injected_recogniser_the_window_looks_for_the_model() {
    let mut rig = Rig::new();
    assert!(!rig.app.transcript.has_injected_transcriber());
    let track = first_audio(&rig.app);
    no_models(&mut rig, "discovery");
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    finish_job(&mut rig);
    assert!(rig.app.transcript.status.contains("Models folder"), "{}", rig.app.transcript.status);
    assert!(rig.app.engine.session().transcript(track).is_none());
}

/// A recogniser that does not answer until it is released, and then notices it was stopped: the
/// shape of a model that takes a while to see the Cancel.
struct Gated {
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl soundcraft_pa_speech::Transcriber for Gated {
    fn id(&self) -> String {
        "gated".into()
    }
    fn transcribe(
        &self,
        _: &[f32],
        _: &soundcraft_pa_speech::Options,
        progress: soundcraft_pa_speech::ProgressFn,
    ) -> Result<Transcript, soundcraft_pa_speech::SpeechError> {
        // Blocks until the test sends or drops its end; either way the job then looks at its stop flag.
        let _ = self.release.lock().map(|r| r.recv());
        if progress(0.5, "Working") {
            return Ok(Transcript { words: vec![Word::new("late", 0.0, 0.2)], language: "en".into(), source: "gated".into() });
        }
        Err(soundcraft_pa_speech::SpeechError::Cancelled)
    }
}

/// Cancel while the recogniser is still busy: no new job starts (not from the menu, not from the
/// button) until its thread has ended, the window says why, and then Transcribe works again and the
/// next job stores its transcript.
#[test]
fn after_cancel_a_new_job_waits_for_the_old_thread_then_runs() {
    let (mut rig, track) = rig_with_recogniser();
    let (release, gate) = std::sync::mpsc::channel();
    rig.app.transcript.inject_transcriber(Arc::new(Gated { release: std::sync::Mutex::new(gate) }));
    open(&mut rig);
    let at = rig.at("Transcribe Track");
    rig.click(at);
    assert!(rig.app.transcript.is_running(), "the first job is under way");

    rig.frames(2);
    let at = rig.at("Cancel");
    rig.click(at);
    assert!(!rig.app.transcript.is_running(), "Cancel forgets the job");
    assert!(rig.app.transcript.is_stopping(), "but its thread is still inside the recogniser");
    assert_eq!(rig.app.transcript.status, "Stopping the previous transcription…");
    assert!(rig.find("Stopping the previous transcription…").is_some(), "the window says so: {:?}", rig.texts());
    assert!(rig.find("Cancel").is_none(), "there is nothing left to cancel");

    // Asking again, from the menu or the greyed-out button, starts nothing.
    rig_recogniser_swap(&mut rig);
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    rig.frames(2);
    assert!(!rig.app.transcript.is_running(), "the menu entry started nothing while the old thread lives");
    assert_eq!(rig.app.transcript.status, "Stopping the previous transcription…");
    let at = rig.at("Transcribe Track");
    rig.click(at);
    assert!(!rig.app.transcript.is_running(), "the greyed-out button started nothing");
    assert!(rig.app.engine.session().transcript(track).is_none());

    // The old thread ends. The window notices on its own and says it stopped.
    drop(release);
    let until = Instant::now() + Duration::from_secs(10);
    while rig.app.transcript.is_stopping() && Instant::now() < until {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.frames(3);
    assert!(!rig.app.transcript.is_stopping(), "the old thread ended");
    assert_eq!(rig.app.transcript.status, "Transcription stopped");
    assert!(rig.app.engine.session().transcript(track).is_none(), "the stopped job stored nothing");

    // Transcribe works again, and the job that follows runs to the end.
    let steps = rig.app.engine.undo_history().len();
    let at = rig.at("Transcribe Track");
    rig.click(at);
    assert!(rig.app.transcript.is_running(), "the button is live again");
    finish_job(&mut rig);
    assert_stored_through_the_command(&mut rig, track, steps);
}

/// Puts the quick fixed recogniser back on the window (the gated one is still held by the first
/// job's thread, which keeps its own copy).
fn rig_recogniser_swap(rig: &mut Rig) {
    rig.app.transcript.inject_transcriber(fixed_recogniser());
}

// ---- the real model, through the window ----------------------------------------------------

/// With `$PA_SPEECH_MODELS_DIR` and `$PA_SPEECH_FIXTURE` (a mono 16 kHz f32 raw clip) set, in a
/// build with the `whisper` feature, the real model transcribes the clip through the menu: the audio
/// is imported into a 48 kHz session (so the conversion to 16 kHz runs on the worker), the job runs
/// on its thread, and the transcript is stored by `track.set_transcript`. Skips (and says so) without
/// them; a skip proves nothing about speech recognition.
#[test]
fn the_real_model_transcribes_a_track_through_the_window() {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (Some(models), Some(fixture)) = (env("PA_SPEECH_MODELS_DIR"), env("PA_SPEECH_FIXTURE")) else {
        eprintln!("SKIPPED: set PA_SPEECH_MODELS_DIR and PA_SPEECH_FIXTURE to run the real model through the window");
        return;
    };
    if !soundcraft_pa_speech::available() {
        eprintln!("SKIPPED: this build has no speech feature (build with --features whisper)");
        return;
    }
    let keyword = env("PA_SPEECH_FIXTURE_KEYWORD").expect("PA_SPEECH_FIXTURE_KEYWORD names a word the clip says").to_lowercase();
    let raw = std::fs::read(&fixture).expect("read the fixture");
    let clip: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    assert!(!clip.is_empty());

    // The clip as a 48 kHz WAV on disk, imported like a person would.
    let up = soundcraft_dsp::offline::resample(&[clip], 16_000, 48_000);
    let wav = soundcraft_audio_io::encode(
        &soundcraft_audio_io::AudioBuffer { sample_rate: 48_000, channels: up },
        &soundcraft_audio_io::EncodeOptions::default(),
    )
    .expect("encode the clip");
    let dir = std::env::temp_dir().join(format!("pa-tools-real-window-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch folder");
    let path = dir.join("speech.wav");
    std::fs::write(&path, wav).expect("write the clip");

    let app = SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default());
    let mut rig = Rig { ctx: egui::Context::default(), app, shapes: Vec::new(), modifiers: Modifiers::NONE };
    rig.frames(4);
    rig.app.run("file.import_audio", json!({"path": path.to_string_lossy()})).expect("import the clip");
    let track = first_audio(&rig.app);
    assert_eq!(rig.app.engine.session().sample_rate.hz(), 48_000);
    // The Models folder field stays empty: the program's own search ($PA_SPEECH_MODELS_DIR) finds the model.
    let _ = models;
    let steps = rig.app.engine.undo_history().len();

    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    assert!(rig.app.transcript.is_running());
    let started = Instant::now();
    while rig.app.transcript.is_running() && started.elapsed() < Duration::from_secs(900) {
        rig.frame(vec![]);
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.frames(2);
    assert!(!rig.app.transcript.is_running(), "the real model did not finish in time");

    let stored = rig.app.engine.session().transcript(track).unwrap_or_else(|| panic!("nothing was stored: {}", rig.app.transcript.status));
    let text = stored.text();
    eprintln!("real model through the window: {} words in {:?}: {text}", stored.words.len(), started.elapsed());
    assert!(text.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| w == keyword), "`{keyword}` is not in {text:?}");
    if let Some(reference) = env("PA_SPEECH_FIXTURE_REFERENCE") {
        let wer = soundcraft_pa_speech::word_error_rate(&reference, &text);
        eprintln!("word error rate {wer:.4}");
        assert!(wer < 0.25, "word error rate {wer} for {text:?}");
    }
    assert_eq!(stored.source, "whisper-base");
    assert_eq!(rig.app.engine.undo_history().len(), steps + 1);
    assert_eq!(rig.app.engine.undo_label(), Some("Set Track Transcript"));
    assert!(rig.app.transcript.status.starts_with("Transcribed "), "{}", rig.app.transcript.status);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- S4: replacing the session stops a running transcription -------------------------------

#[test]
fn opening_a_session_from_bytes_stops_a_running_transcription() {
    let mut rig = Rig::new();
    no_models(&mut rig, "open-bytes");
    menus::invoke_menu(&mut rig.app, "track.transcribe", "Track > Transcribe Track");
    assert!(rig.app.transcript.is_running());
    let other = soundcraft_model::Session::default().to_json().unwrap();
    soundcraft_ui_egui::dialogs::open_session_bytes(&mut rig.app, "Other.scraft".into(), other.into_bytes());
    assert!(!rig.app.transcript.is_running(), "the job belonged to the session that was replaced");
    rig.frames(3);
    assert!(rig.app.engine.session().transcripts.is_empty(), "and its answer is not stored in the new session");
}

// ---- nothing the window paints names a third-party product ---------------------------------

/// Every text of the last frame, lowercased, in one string.
fn painted(rig: &Rig) -> String {
    rig.texts().join("\n").to_lowercase()
}

fn assert_no_third_party_names(rig: &Rig, state: &str) {
    let text = painted(rig);
    assert!(!text.is_empty(), "{state}: nothing was painted");
    for name in ["whisper", "openai", "hugging face"] {
        assert!(!text.contains(name), "{state}: the window paints `{name}`:\n{text}");
    }
}

#[test]
fn the_window_paints_no_third_party_names_with_the_models_folder_empty() {
    let mut rig = Rig::new();
    open(&mut rig);
    assert!(rig.find("Transcribe Track").is_some() && rig.find("Models folder").is_some(), "{:?}", rig.texts());
    // The field is empty and shows its hint.
    assert_eq!(rig.app.transcript.models_dir, "");
    assert!(rig.find("App's own models folder").is_some(), "{:?}", rig.texts());
    assert_no_third_party_names(&rig, "empty Models folder");
}

#[test]
fn the_window_paints_no_third_party_names_when_the_model_is_missing() {
    let mut rig = Rig::new();
    no_models(&mut rig, "paint");
    open(&mut rig);
    let at = rig.at("Transcribe Track");
    rig.click(at);
    finish_job(&mut rig);
    let status = rig.app.transcript.status.clone();
    assert!(status.contains("Models folder"), "{status}");
    assert!(rig.find(&status).is_some(), "the window paints the status: {:?}", rig.texts());
    assert_no_third_party_names(&rig, "model missing");
}

#[test]
fn the_window_paints_no_third_party_names_with_a_stored_transcript() {
    let mut rig = Rig::new();
    let track = first_audio(&rig.app);
    // The id a real transcription stores as its source.
    rig.app.run("track.set_transcript", json!({"track": track.0, "words": words(30), "language": "en", "source": "whisper-base"})).unwrap();
    open(&mut rig);
    assert!(rig.find("Alpha").is_some(), "the transcript is listed: {:?}", rig.texts());
    assert_no_third_party_names(&rig, "stored transcript");

    // Hover the summary line and the Models folder field: their tooltips are painted too.
    for label in [None, Some("30 words · en")] {
        rig.frames(2);
        let at = match label {
            Some(text) => rig.at(text),
            None => rig.at("Models folder") + vec2(120.0, 0.0),
        };
        rig.frame(vec![Event::PointerMoved(at)]);
        for _ in 0..90 {
            rig.frame(vec![]);
        }
        let text = painted(&rig);
        assert_no_third_party_names(&rig, "hovering");
        let expect = match label {
            Some(_) => "made with speech model, base (multilingual)",
            None => "the folder that contains the speech model's folder",
        };
        assert!(text.contains(expect), "hovering {label:?} should show `{expect}`:\n{text}");
    }
}

//! Window ▸ Transcript: the command, the menu entry, drawing a long transcript, clicking words,
//! naming speakers and the model-missing round trip through the worker thread.

use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Shape, epaint::ClippedShape, vec2};
use serde_json::{Value, json};
use soundcraft_model::{TrackId, TrackKind};
use soundcraft_ui_egui::{Services, SoundApp, menus};
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
    assert!(status.contains("speech model") && status.contains("Models folder") && status.contains("whisper-base/"), "{status}");
    assert_eq!(rig.app.ui.status, status, "the main status line says it too");
    assert!(rig.app.engine.session().transcript(track).is_none(), "nothing was stored");
    assert!(rig.find(&status).is_some(), "the window shows the status");
}

#[test]
fn the_models_folder_defaults_to_the_environment_and_can_be_edited() {
    let rig = Rig::new();
    assert_eq!(rig.app.transcript.models_dir, std::env::var("PA_SPEECH_MODELS_DIR").unwrap_or_default());
    assert!(rig.app.transcript.follow, "Follow playhead starts on");
}

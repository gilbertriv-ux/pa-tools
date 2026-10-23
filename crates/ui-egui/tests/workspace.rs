use egui::{Rect, Vec2, pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable as _};
use soundcraft_ui_egui::{MainWindow, Services, SoundApp, dialogs::Dialog};

fn harness(demo: bool, size: Vec2) -> Harness<'static, SoundApp> {
    let engine = if demo { soundcraft_engine::demo::demo_engine() } else { soundcraft_engine::Engine::default() };
    let app = SoundApp::new(engine, None, Services::default());
    let mut h = Harness::builder().with_size(size).build_ui_state(
        |ui, app: &mut SoundApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(8);
    h
}

#[test]
fn workspace_navigation_and_transcript_hosts_preserve_the_session() {
    let mut h = harness(true, vec2(1440.0, 1000.0));
    let before = serde_json::to_value(h.state().engine.session()).unwrap();
    h.get_by_label("Mix Console").click();
    h.run_steps(6);
    assert_eq!(h.state().ui.window, MainWindow::Mix);
    h.get_by_label("Edit Timeline").click();
    h.run_steps(6);
    assert_eq!(h.state().ui.window, MainWindow::Edit);
    h.get_by_label("Transcript").click();
    h.run_steps(6);
    h.get_by_label("Transcript Inspector");
    assert!(h.state().ui.transcript.docked);
    h.get_by_label("Detach").click();
    h.run_steps(6);
    assert!(!h.state().ui.transcript.docked);
    h.get_by_label("Dock Inspector").click();
    h.run_steps(6);
    h.get_by_label("Transcript Inspector");
    assert!(h.state().ui.transcript.docked);
    assert_eq!(serde_json::to_value(h.state().engine.session()).unwrap(), before);
}

#[test]
fn empty_session_actions_open_the_existing_dialogs() {
    let mut h = harness(false, vec2(1200.0, 800.0));
    h.get_by_label("Start your session");
    h.get_by_label("Create an Audio Track").click();
    h.run_steps(6);
    assert!(matches!(h.state().dialogs.open, Some(Dialog::NewTracks { .. })));
    h.state_mut().dialogs.open = None;
    h.run_steps(6);
    h.get_by_label("Import an Audio File").click();
    h.run_steps(6);
    assert!(matches!(&h.state().dialogs.open, Some(Dialog::PathPrompt { cmd, .. }) if cmd == "file.import_audio"));
    assert!(h.state().engine.session().tracks.is_empty());
}

#[test]
fn small_desktop_windows_keep_navigation_and_a_floating_transcript_accessible() {
    let size = vec2(800.0, 620.0);
    let mut h = harness(false, size);
    let viewport = Rect::from_min_size(pos2(0.0, 0.0), size);
    for name in ["Edit Timeline", "Mix Console", "Transcript", "Add Track", "Import Audio", "Open Session", "Save"] {
        assert!(viewport.contains_rect(h.get_by_label(name).rect()), "{name} is outside the viewport");
    }
    h.get_by_label("Transcript").click();
    h.run_steps(8);
    assert!(h.query_by_label("Transcript Inspector").is_none());
    let button = h.get_by_label("Transcribe Track").rect();
    assert!(viewport.contains_rect(button));
}

#[test]
fn docked_transcript_wraps_whole_words_without_overlapping_the_clip_panel() {
    let mut h = harness(true, vec2(1440.0, 1000.0));
    let track = h.state().engine.session().tracks.first().unwrap().id;
    let long_word = "x".repeat(300);
    let mut words: Vec<_> =
        (0..30).map(|i| soundcraft_model::TranscriptWord { start: i * 9600, end: (i + 1) * 9600, text: format!("word{i:02}"), speaker: 0 }).collect();
    words.push(soundcraft_model::TranscriptWord { start: 288_000, end: 297_600, text: long_word.clone(), speaker: 0 });
    h.state_mut().engine.session_mut().set_transcript(soundcraft_model::Transcript {
        track,
        language: "en".into(),
        source: "layout fixture".into(),
        words,
    });
    h.state_mut().run("window.transcript", serde_json::json!({"value":true,"docked":true})).unwrap();
    h.run_steps(8);
    // The leftmost text input in this fixture is the Clips panel search.
    let clips = h.query_all_by_role(egui::accesskit::Role::TextInput).map(|node| node.rect()).min_by(|a, b| a.min.x.total_cmp(&b.min.x)).unwrap();
    let title = h.get_by_label("Transcript Inspector").rect();
    assert!(title.min.x > clips.max.x, "inspector title {title:?} overlaps Clip search {clips:?}");
    let first = h.get_by_label("word00").rect();
    let last = h.get_by_label("word29").rect();
    assert!(last.min.y > first.min.y, "words did not wrap onto another row");
    for i in 0..30 {
        let rect = h.get_by_label(&format!("word{i:02}")).rect();
        assert!(rect.height() <= 30.0, "word{i:02} was broken vertically: {rect:?}");
        assert!(rect.min.x > clips.max.x && rect.max.x < 1440.0, "word{i:02} left its reserved panel: {rect:?}");
    }
    let rect = h.get_by_label(&long_word).rect();
    assert!(rect.height() <= 30.0 && rect.min.x > clips.max.x && rect.max.x < 1440.0, "oversized token escaped its panel: {rect:?}");
}

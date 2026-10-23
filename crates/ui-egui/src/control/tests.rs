use super::*;

fn app() -> SoundApp {
    SoundApp::new(soundcraft_engine::Engine::default(), None, crate::Services::default())
}

fn enqueue(app: &mut SoundApp, ctx: &egui::Context) -> Receiver<Value> {
    let (request, reply) = ControlRequest::new("ui.screenshot", json!({}));
    assert!(handle(app, ctx, &request).is_none());
    reply
}

#[test]
fn screenshot_restores_and_focuses_its_window() {
    let ctx = egui::Context::default();
    let mut app = app();
    let output = ctx.run_logic(&egui::RawInput::default(), |ctx| {
        let _reply = enqueue(&mut app, ctx);
    });
    let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
    assert_eq!(commands.as_slice(), &[egui::ViewportCommand::Minimized(false), egui::ViewportCommand::Focus, egui::ViewportCommand::Visible(true)]);
}

#[test]
fn hidden_logic_ticks_do_not_advance_or_expire_a_screenshot() {
    let ctx = egui::Context::default();
    let mut app = app();
    let reply = enqueue(&mut app, &ctx);
    // More than the old 600-logic-call deadline; no frame has been rendered.
    for _ in 0..650 {
        let output = ctx.run_logic(&egui::RawInput::default(), |ctx| collect_screenshots(&mut app, ctx));
        assert!(!output.viewport_commands.values().flatten().any(|c| matches!(c, egui::ViewportCommand::Screenshot(_))));
    }
    assert!(matches!(reply.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    assert_eq!(app.pending_shots.len(), 1);
    assert_eq!(app.pending_shots[0].frames, 0);
}

#[test]
fn screenshot_waits_for_three_ui_frames_and_does_not_retry_early() {
    let ctx = egui::Context::default();
    let mut app = app();
    let _reply = enqueue(&mut app, &ctx);
    let now = Instant::now();
    let mut readbacks = 0;
    for frame in 1..=5 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_screenshots_at(&mut app, ui.ctx(), now);
            // A discarded/repeated layout pass must not advance the request twice.
            request_screenshots_at(&mut app, ui.ctx(), now);
        });
        output.textures_delta.clear();
        readbacks += output.viewport_output.values().flat_map(|v| &v.commands).filter(|c| matches!(c, egui::ViewportCommand::Screenshot(_))).count();
        assert_eq!(readbacks, usize::from(frame >= 3));
    }
}

#[test]
fn screenshot_retries_dropped_readbacks_at_bounded_intervals_with_the_same_token() {
    let ctx = egui::Context::default();
    let mut app = app();
    let _reply = enqueue(&mut app, &ctx);
    let token = app.pending_shots[0].token;
    let start = Instant::now();
    let mut readbacks = 0;
    for elapsed_ms in [0, 0, 0, 249, 250, 499, 500, 750, 1000, 15_000] {
        let now = start + Duration::from_millis(elapsed_ms);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_screenshots_at(&mut app, ui.ctx(), now);
            request_screenshots_at(&mut app, ui.ctx(), now);
        });
        output.textures_delta.clear();
        for command in output.viewport_output.values().flat_map(|v| &v.commands) {
            if let egui::ViewportCommand::Screenshot(data) = command {
                assert_eq!(data.data.as_ref().and_then(|v| v.downcast_ref::<u64>()), Some(&token));
                readbacks += 1;
            }
        }
        let expected = if app.pending_shots[0].frames < 3 { 0 } else { (elapsed_ms / 250 + 1).min(4) };
        assert_eq!(readbacks, expected);
    }
    assert_eq!(app.pending_shots[0].attempts, SCREENSHOT_ATTEMPTS);
}

#[test]
fn screenshot_readback_matches_the_request_token_and_replies_once() {
    let ctx = egui::Context::default();
    let mut app = app();
    let first = enqueue(&mut app, &ctx);
    let second = enqueue(&mut app, &ctx);
    let token = app.pending_shots[1].token;
    let image = std::sync::Arc::new(egui::ColorImage::filled([2, 3], egui::Color32::RED));
    let input = egui::RawInput {
        events: vec![egui::Event::Screenshot { viewport_id: egui::ViewportId::ROOT, user_data: egui::UserData::new(token), image }],
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| collect_screenshots(&mut app, ui.ctx()));
    output.textures_delta.clear();
    let result = second.try_recv().unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["width"], 2);
    assert_eq!(result["result"]["height"], 3);
    assert!(result["result"]["png_base64"].as_str().unwrap().starts_with("iVBOR"));
    assert!(matches!(first.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    assert!(second.try_recv().is_err());
    assert_eq!(app.pending_shots.len(), 1);
}

#[test]
fn screenshot_timeout_uses_elapsed_time_and_drops_the_request() {
    let ctx = egui::Context::default();
    let mut app = app();
    let reply = enqueue(&mut app, &ctx);
    app.pending_shots[0].started = Instant::now().checked_sub(SCREENSHOT_TIMEOUT).unwrap();
    collect_screenshots(&mut app, &ctx);
    let result = reply.try_recv().unwrap();
    assert_eq!(result["ok"], false);
    assert!(result["error"].as_str().unwrap().contains("15 seconds"));
    assert!(app.pending_shots.is_empty());
    collect_screenshots(&mut app, &ctx);
    assert!(reply.try_recv().is_err());
}

#[test]
fn base64_known() {
    assert_eq!(super::base64(b"Man"), "TWFu");
    assert_eq!(super::base64(b"Ma"), "TWE=");
    assert_eq!(super::base64(b"M"), "TQ==");
}

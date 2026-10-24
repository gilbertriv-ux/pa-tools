//! Recording MIDI along with the transport: which tracks take it, a take in progress, and the
//! step that turns it into clips.
//!
//! The hardware side is [`soundcraft_playback::midi_io`]; the take itself is
//! [`soundcraft_playback::midi_record::MidiTake`]. This module owns the parts between them and
//! the session, and none of it needs a device: [`MidiCapture::ingest`] and
//! [`MidiCapture::finish`] take plain [`TimedMessage`]s, which is what the tests feed.
//!
//! Timing: the backend's clock has a different epoch on every platform, so the take is created
//! on the first drain after a message arrives, from the input's [`ClockAnchor`] mapping of the
//! instant recording started onto the backend clock. A take where nothing arrives makes no clip.

use serde_json::Value;
use soundcraft_midi::{PPQ, Sequence};
use soundcraft_model::{Session, TrackId};
use soundcraft_playback::midi_io::{self, MAX_QUEUE, MidiInput, TimedMessage};
use soundcraft_playback::midi_record::{DEFAULT_MAX_EVENTS, MidiTake, TakeStats};
use soundcraft_time::Samples;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::SoundApp;

/// `setup.midi_input`: `{port?: string|null}` selects the MIDI input that records with the
/// transport (an index, a name or part of one); empty or null selects none. Returns `{port}`.
/// Kept with the preferences (per machine); none by default.
pub const INPUT: &str = "setup.midi_input";

/// Messages taken from the queue per frame at most (the queue itself holds [`MAX_QUEUE`]).
const DRAIN_PER_FRAME: usize = MAX_QUEUE;

pub fn run(app: &mut SoundApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != INPUT {
        return None;
    }
    if let Some(v) = p.get("port") {
        app.ui.midi_input = v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    }
    Some(Ok(serde_json::json!({"port": app.ui.midi_input})))
}

/// The record-enabled MIDI and instrument tracks, which take the MIDI that is played in.
pub fn armed_midi_tracks(s: &Session) -> Vec<TrackId> {
    s.tracks.iter().filter(|t| t.mixer.record_arm && t.kind.is_midi() && !t.inactive).map(|t| t.id).collect()
}

/// Whether any record-enabled audio track will record a take file.
pub fn any_armed_audio(s: &Session) -> bool {
    s.tracks.iter().any(|t| t.mixer.record_arm && t.kind == soundcraft_model::TrackKind::Audio && !t.inactive)
}

/// An open port and its queue.
pub struct Live {
    input: MidiInput,
    rx: Receiver<TimedMessage>,
}

impl Live {
    /// Open `port` (an index, a name or part of one) for a take; the error is ready to show.
    pub fn open(port: &str) -> Result<Live, String> {
        let (input, rx) = midi_io::open_input(port, MAX_QUEUE).map_err(|e| e.to_string())?;
        Ok(Live { input, rx })
    }

    /// The port's name as the system reports it.
    pub fn name(&self) -> &str {
        self.input.port_name()
    }
}

/// What a finished take places: where, how long, on which tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub sequence: Sequence,
    pub stats: TakeStats,
    pub tracks: Vec<TrackId>,
    pub start: Samples,
    pub len: Samples,
}

/// A MIDI take in progress.
pub struct MidiCapture {
    live: Option<Live>,
    tracks: Vec<TrackId>,
    started: Instant,
    start: Samples,
    sample_rate: u32,
    tempo_bpm: f64,
    start_micros: Option<u64>,
    take: Option<MidiTake>,
    /// Messages that arrived before the clock could be matched (should stay 0).
    unanchored: usize,
}

impl MidiCapture {
    /// Begin a take on `tracks` at timeline position `start` (samples), at `tempo_bpm`.
    /// `live` is the open port; tests pass `None` and call [`Self::ingest`] themselves.
    pub fn new(live: Option<Live>, tracks: Vec<TrackId>, start: Samples, sample_rate: u32, tempo_bpm: f64, started: Instant) -> Self {
        MidiCapture { live, tracks, started, start, sample_rate, tempo_bpm, start_micros: None, take: None, unanchored: 0 }
    }

    /// The tracks that get a clip when the take finishes.
    pub fn tracks(&self) -> &[TrackId] {
        &self.tracks
    }

    /// Take what the port has queued. Returns how many messages that was.
    pub fn drain(&mut self) -> usize {
        let Some(live) = &self.live else { return 0 };
        let msgs: Vec<TimedMessage> = live.rx.try_iter().take(DRAIN_PER_FRAME).collect();
        // After the messages are in hand, so the first one has set the anchor.
        let start = live.input.micros_at(self.started);
        let n = msgs.len();
        self.ingest(start, msgs);
        n
    }

    /// Feed messages into the take. `start_micros` is the backend timestamp of the instant
    /// recording started; the take is created from the first value given, and messages given
    /// while it is still `None` are dropped and counted.
    pub fn ingest(&mut self, start_micros: Option<u64>, msgs: impl IntoIterator<Item = TimedMessage>) {
        if self.start_micros.is_none() {
            self.start_micros = start_micros;
        }
        if self.take.is_none()
            && let Some(at) = self.start_micros
        {
            self.take = MidiTake::new(at, self.tempo_bpm, PPQ, 0, DEFAULT_MAX_EVENTS).ok();
        }
        for m in msgs {
            match &mut self.take {
                Some(t) => t.push_message(&m),
                None => self.unanchored = self.unanchored.saturating_add(1),
            }
        }
    }

    /// Stop `elapsed_micros` after the start: takes what is left in the queue, closes notes still
    /// sounding, and returns what to place. Nothing to place when no message arrived.
    pub fn finish(mut self, elapsed_micros: u64) -> Outcome {
        self.drain();
        let stop = self.start_micros.map(|s| s.saturating_add(elapsed_micros));
        let (sequence, stats) = match (self.take.take(), stop) {
            (Some(t), Some(stop)) => t.finish(stop),
            _ => (Sequence::default(), TakeStats::default()),
        };
        let len = i128::from(elapsed_micros) * i128::from(self.sample_rate) / 1_000_000;
        Outcome { sequence, stats, tracks: self.tracks, start: self.start, len: Samples::try_from(len).unwrap_or(Samples::MAX).max(1) }
    }

    /// [`Self::finish`] at the current time.
    pub fn finish_now(self) -> Outcome {
        let elapsed = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.finish(elapsed)
    }
}

/// What the status line says about a finished take.
pub fn summary(clips: usize, stats: &TakeStats) -> String {
    let events = stats.notes.saturating_add(stats.ctrls);
    let mut s = format!("{clips} MIDI clip(s) with {} note(s) and {} controller event(s)", stats.notes, stats.ctrls);
    let lost = stats.dropped_cap.saturating_add(stats.dropped_overflow);
    if lost > 0 {
        s.push_str(&format!(" ({lost} event(s) over the limit were not kept)"));
    }
    if events == 0 {
        s.push_str(" (nothing was played)");
    }
    s
}

impl SoundApp {
    /// Open the selected MIDI input for a take, when one is selected and a record-enabled MIDI or
    /// instrument track exists. `Ok(None)` when there is nothing to do; `Err` is a message ready
    /// to show when the port cannot be opened.
    pub(crate) fn open_midi_input(&mut self) -> Result<Option<(Live, Vec<TrackId>)>, String> {
        let tracks = armed_midi_tracks(self.engine.session());
        let Some(port) = self.ui.midi_input.clone().filter(|p| !p.trim().is_empty()) else { return Ok(None) };
        if tracks.is_empty() {
            return Ok(None);
        }
        Live::open(&port).map(|live| Some((live, tracks))).map_err(|e| format!("Cannot open MIDI input “{port}”: {e}"))
    }

    /// Begin the take at the recording's start position, at the session's tempo there.
    pub(crate) fn begin_midi_capture(&mut self, live: Option<(Live, Vec<TrackId>)>) {
        self.midi_capture = None;
        let Some((live, tracks)) = live else { return };
        let (rate, tempo) = {
            let s = self.engine.session();
            (s.sample_rate, s.tempo.tempo_at_tick(s.tempo.samples_to_ticks(self.record_start, s.sample_rate)))
        };
        let capture = MidiCapture::new(Some(live), tracks, self.record_start, rate.hz(), tempo, Instant::now());
        // A tempo the take cannot use is reported instead of recording nothing in silence.
        if soundcraft_playback::midi_io::micros_to_ticks(0, tempo, PPQ).is_none() {
            self.ui.status = "Cannot record MIDI: the session tempo is out of range.".into();
            return;
        }
        self.midi_capture = Some(capture);
    }

    /// Each frame while recording: move what the port received into the take.
    pub(crate) fn drain_midi(&mut self) {
        if let Some(c) = &mut self.midi_capture {
            c.drain();
        }
    }

    /// End the take in progress, if any.
    pub(crate) fn finish_midi_capture(&mut self) -> Option<Outcome> {
        self.midi_capture.take().map(MidiCapture::finish_now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use soundcraft_engine::Engine;
    use soundcraft_model::{ChannelFormat, TrackKind};

    fn msg(micros: u64, bytes: &[u8]) -> TimedMessage {
        TimedMessage::new(micros, bytes).expect("short message")
    }

    /// 120 bpm at 960 ticks per quarter: one tick is 520.83 us, one beat 500_000 us.
    fn capture(tracks: Vec<TrackId>) -> MidiCapture {
        MidiCapture::new(None, tracks, 48_000, 48_000, 120.0, Instant::now())
    }

    #[test]
    fn only_armed_midi_and_instrument_tracks_take_midi() {
        let mut e = Engine::default();
        let keys = e.session_mut().add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        let pads = e.session_mut().add_track(TrackKind::Midi, ChannelFormat::Stereo, Some("Pads"));
        let _unarmed = e.session_mut().add_track(TrackKind::Midi, ChannelFormat::Stereo, Some("Lead"));
        let vox = e.session_mut().add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Vox"));
        assert!(armed_midi_tracks(e.session()).is_empty());
        for t in [keys, pads, vox] {
            e.execute("mix.record_arm", &json!({"tracks": [t.0], "value": true})).unwrap();
        }
        assert_eq!(armed_midi_tracks(e.session()), vec![keys, pads]);
        assert!(any_armed_audio(e.session()));
    }

    #[test]
    fn injected_messages_become_a_sequence_at_the_start_tick() {
        let mut c = capture(vec![TrackId(1)]);
        // The clock is matched to the backend epoch 10 s in; middle C for one beat, a pedal press.
        let start = 10_000_000;
        c.ingest(
            Some(start),
            [msg(start + 500_000, &[0x90, 60, 100]), msg(start + 600_000, &[0xB0, 64, 127]), msg(start + 1_000_000, &[0x80, 60, 0])],
        );
        let out = c.finish(2_000_000);
        assert_eq!(out.tracks, vec![TrackId(1)]);
        assert_eq!((out.start, out.len), (48_000, 96_000), "starts where recording did, lasts as long as it ran");
        assert_eq!(out.sequence.notes.len(), 1);
        let n = out.sequence.notes[0];
        assert_eq!((n.pitch, n.velocity, n.start, n.length), (60, 100, 960, 960));
        assert_eq!(out.sequence.ctrls.len(), 1);
        assert_eq!(out.sequence.ctrls[0].tick, 1152, "0.6 s at 120 bpm");
        assert_eq!((out.stats.notes, out.stats.ctrls, out.stats.stuck_closed), (1, 1, 0));
    }

    #[test]
    fn ingest_takes_messages_in_several_batches() {
        let mut c = capture(vec![TrackId(1)]);
        c.ingest(Some(0), [msg(0, &[0x90, 60, 100])]);
        c.ingest(Some(0), [msg(500_000, &[0x80, 60, 0]), msg(500_000, &[0x90, 62, 80])]);
        let out = c.finish(1_000_000);
        assert_eq!(out.sequence.notes.len(), 2);
        assert_eq!(out.stats.stuck_closed, 1, "a note still sounding at stop is closed there");
        assert_eq!(out.sequence.notes.iter().find(|n| n.pitch == 62).map(|n| n.length), Some(960));
    }

    #[test]
    fn nothing_played_leaves_an_empty_take_and_unmatched_messages_are_dropped() {
        let c = capture(vec![TrackId(1)]);
        let out = c.finish(1_000_000);
        assert!(out.sequence.notes.is_empty() && out.sequence.ctrls.is_empty());
        let mut c = capture(vec![TrackId(1)]);
        c.ingest(None, [msg(5, &[0x90, 60, 100])]);
        assert_eq!(c.unanchored, 1);
        assert!(c.finish(1_000).sequence.notes.is_empty());
    }

    #[test]
    fn a_finished_take_lands_on_the_armed_tracks_as_one_undo_step() {
        let mut app = SoundApp::new(Engine::default(), None, crate::Services::default());
        let keys = app.engine.session_mut().add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        let pads = app.engine.session_mut().add_track(TrackKind::Midi, ChannelFormat::Stereo, Some("Pads"));
        let off = app.engine.session_mut().add_track(TrackKind::Midi, ChannelFormat::Stereo, Some("Off"));
        for t in [keys, pads] {
            app.engine.execute("mix.record_arm", &json!({"tracks": [t.0], "value": true})).unwrap();
        }
        let mut c = MidiCapture::new(None, armed_midi_tracks(app.engine.session()), 96_000, 48_000, 120.0, Instant::now());
        c.ingest(Some(0), [msg(0, &[0x90, 64, 90]), msg(250_000, &[0x80, 64, 0])]);
        let before = app.engine.undo_history().len();
        let out = c.finish(1_000_000);
        let ids = soundcraft_engine::io::place_midi_recording(&mut app.engine, out.start, out.len, &out.tracks, &out.sequence, true);
        assert_eq!(ids.len(), 2);
        assert_eq!(app.engine.undo_history().len(), before + 1);
        for t in [keys, pads] {
            let clip = app.engine.session().track(t).and_then(|t| t.playlist()).and_then(|p| p.clips.first()).cloned().unwrap();
            assert_eq!((clip.start, clip.length), (96_000, 48_000));
            assert!(
                matches!(&clip.content, soundcraft_model::ClipContent::Midi { sequence } if sequence.notes.len() == 1 && sequence.notes[0].length == 480)
            );
        }
        assert!(app.engine.session().track(off).and_then(|t| t.playlist()).is_some_and(|p| p.clips.is_empty()), "an unarmed track gets nothing");
        app.engine.execute("edit.undo", &json!({})).unwrap();
        assert!(ids.iter().all(|id| app.engine.session().find_clip(*id).is_none()));
    }

    #[test]
    fn the_port_choice_is_a_command_and_defaults_to_none() {
        let mut app = SoundApp::new(Engine::default(), None, crate::Services::default());
        assert_eq!(app.ui.midi_input, None);
        assert_eq!(app.run(INPUT, json!({"port": " Keys "})).unwrap(), json!({"port": "Keys"}));
        assert_eq!(app.ui.midi_input.as_deref(), Some("Keys"));
        assert_eq!(app.run(INPUT, json!({})).unwrap(), json!({"port": "Keys"}), "no `port` reads it");
        assert_eq!(app.run(INPUT, json!({"port": ""})).unwrap(), json!({"port": null}));
        assert_eq!(app.run(INPUT, json!({"port": "Keys"})).and_then(|_| app.run(INPUT, json!({"port": null}))).unwrap(), json!({"port": null}));
        let ui: crate::UiState = serde_json::from_value(json!({"midi_input": "Keys"})).unwrap();
        assert_eq!(ui.midi_input.as_deref(), Some("Keys"), "kept with the preferences");
        let old: crate::UiState = serde_json::from_value(json!({})).unwrap();
        assert_eq!(old.midi_input, None, "preferences saved before this one load unchanged");
    }

    #[test]
    fn summary_says_what_was_kept() {
        let stats = TakeStats { notes: 3, ctrls: 2, ..TakeStats::default() };
        assert_eq!(summary(2, &stats), "2 MIDI clip(s) with 3 note(s) and 2 controller event(s)");
        assert!(summary(1, &TakeStats { dropped_cap: 4, notes: 1, ..TakeStats::default() }).contains("4 event(s) over the limit"));
    }

    #[test]
    fn stopping_places_the_take_with_the_audio_undo_step_and_says_so() {
        let mut app = SoundApp::new(Engine::default(), None, crate::Services::default());
        let keys = app.engine.session_mut().add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        app.engine.execute("mix.record_arm", &json!({"tracks": [keys.0], "value": true})).unwrap();
        app.record_start = 48_000;
        let mut c = MidiCapture::new(None, vec![keys], 48_000, 48_000, 120.0, Instant::now());
        c.ingest(Some(0), [msg(0, &[0x90, 60, 100]), msg(100_000, &[0x80, 60, 0])]);
        app.midi_capture = Some(c);
        app.engine.transport.recording = true;
        let steps = app.engine.undo_history().len();
        app.finish_recording();
        assert!(!app.engine.transport.recording && app.midi_capture.is_none());
        assert_eq!(app.engine.undo_history().len(), steps + 1);
        assert_eq!(app.engine.undo_label(), Some("Record"));
        assert!(app.ui.status.starts_with("Recorded 1 MIDI clip(s) with 1 note(s)"), "{}", app.ui.status);
    }

    #[test]
    fn no_port_or_no_armed_midi_track_changes_nothing() {
        let mut app = SoundApp::new(Engine::default(), None, crate::Services::default());
        let keys = app.engine.session_mut().add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        // Armed but no port: no capture, and the old "cannot record" answer for a session with no audio input.
        app.engine.execute("mix.record_arm", &json!({"tracks": [keys.0], "value": true})).unwrap();
        assert!(matches!(app.open_midi_input(), Ok(None)));
        app.run("transport.record", json!({})).unwrap();
        app.handle_transport(0.25);
        assert!(!app.engine.transport.recording && app.midi_capture.is_none() && app.pending_midi.is_none());
        assert!(app.ui.status.starts_with("Cannot record:"), "{}", app.ui.status);
        // A port but nothing armed.
        app.engine.execute("mix.record_arm", &json!({"tracks": [keys.0], "value": false})).unwrap();
        app.run(INPUT, json!({"port": "Keys"})).unwrap();
        assert!(matches!(app.open_midi_input(), Ok(None)));
        let steps = app.engine.undo_history().len();
        app.finish_recording();
        assert_eq!(app.engine.undo_history().len(), steps, "nothing recorded, nothing placed");
    }

    #[test]
    fn a_port_that_cannot_be_opened_is_a_message_not_a_panic() {
        let mut app = SoundApp::new(Engine::default(), None, crate::Services::default());
        let keys = app.engine.session_mut().add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
        app.engine.execute("mix.record_arm", &json!({"tracks": [keys.0], "value": true})).unwrap();
        app.run(INPUT, json!({"port": "no such port anywhere"})).unwrap();
        let err = app.open_midi_input().err().unwrap_or_default();
        assert!(err.starts_with("Cannot open MIDI input “no such port anywhere”"), "{err}");
        app.run("transport.record", json!({})).unwrap();
        app.handle_transport(0.25);
        assert!(!app.engine.transport.recording);
        assert!(app.ui.status.contains("Cannot open MIDI input"), "{}", app.ui.status);
    }
}

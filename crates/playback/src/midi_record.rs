//! Pure MIDI take recorder core: turns timestamped input messages into a [`Sequence`].
//!
//! No hardware is involved. Feed it [`TimedMessage`]s (or parsed [`MidiEvent`]s with a
//! timestamp) from [`crate::midi_io`], then call [`MidiTake::finish`]. Everything is bounded:
//! the number of stored notes plus controller events never exceeds the configured cap, and
//! anything past it is counted in [`TakeStats`] instead of stored.
//!
//! Timing: microsecond timestamps are made relative to the transport start and converted with
//! [`micros_to_ticks`] at a constant tempo. Ticks are those of the `ppq` given (pass
//! [`soundcraft_midi::PPQ`] to get model ticks). A latency offset in ticks is subtracted
//! (saturating at tick 0), so a positive offset moves recorded events earlier. Timestamps that
//! go backwards are clamped to the previous event's tick, so output is monotonic.

use std::collections::{HashMap, VecDeque};

use soundcraft_midi::{CtrlEvent, CtrlKind, Note, Sequence};

use crate::midi_io::{MidiEvent, TimedMessage, micros_to_ticks};

/// Default cap on stored notes plus controller events in one take.
pub const DEFAULT_MAX_EVENTS: usize = 1_000_000;
/// Hard upper bound accepted for the cap.
pub const MAX_EVENTS_LIMIT: usize = 4_000_000;

/// Why a take could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// Tempo not finite, not positive, or above the supported maximum.
    InvalidTempo,
    /// Resolution of zero ticks per quarter note.
    InvalidPpq,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTempo => write!(f, "invalid tempo for MIDI recording"),
            Self::InvalidPpq => write!(f, "invalid resolution for MIDI recording"),
        }
    }
}

impl std::error::Error for RecordError {}

/// Counters describing what a take kept and discarded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TakeStats {
    /// Notes in the result.
    pub notes: usize,
    /// Controller events in the result.
    pub ctrls: usize,
    /// Notes still sounding at stop that were closed there.
    pub stuck_closed: usize,
    /// Note-offs with no matching note-on (ignored).
    pub orphan_offs: usize,
    /// Events dropped because the cap was reached.
    pub dropped_cap: usize,
    /// Events stamped before the transport start (ignored).
    pub dropped_early: usize,
    /// Events whose time did not convert to a tick (overflow).
    pub dropped_overflow: usize,
    /// Messages that did not parse or are not recorded (SysEx, clock...).
    pub ignored: usize,
}

#[derive(Debug, Clone, Copy)]
struct OpenNote {
    start: i64,
    velocity: u8,
}

/// An in-progress recording.
#[derive(Debug, Clone)]
pub struct MidiTake {
    start_micros: u64,
    tempo_bpm: f64,
    ppq: u32,
    latency_ticks: i64,
    max_events: usize,
    last_tick: i64,
    open: HashMap<(u8, u8), VecDeque<OpenNote>>,
    open_count: usize,
    notes: Vec<Note>,
    ctrls: Vec<CtrlEvent>,
    stats: TakeStats,
}

impl MidiTake {
    /// Start a take. `start_micros` is the backend timestamp of transport start;
    /// `latency_ticks` is subtracted from every event time (saturating at 0); `max_events` is
    /// clamped to `1..=`[`MAX_EVENTS_LIMIT`].
    pub fn new(start_micros: u64, tempo_bpm: f64, ppq: u32, latency_ticks: i64, max_events: usize) -> Result<Self, RecordError> {
        if ppq == 0 {
            return Err(RecordError::InvalidPpq);
        }
        if micros_to_ticks(0, tempo_bpm, ppq).is_none() {
            return Err(RecordError::InvalidTempo);
        }
        Ok(Self {
            start_micros,
            tempo_bpm,
            ppq,
            latency_ticks,
            max_events: max_events.clamp(1, MAX_EVENTS_LIMIT),
            last_tick: 0,
            open: HashMap::new(),
            open_count: 0,
            notes: Vec::new(),
            ctrls: Vec::new(),
            stats: TakeStats::default(),
        })
    }

    /// Statistics so far (`notes`/`ctrls`/`stuck_closed` are filled in by [`Self::finish`]).
    pub fn stats(&self) -> TakeStats {
        self.stats
    }

    fn stored(&self) -> usize {
        self.notes.len().saturating_add(self.ctrls.len()).saturating_add(self.open_count)
    }

    fn tick_at(&mut self, micros: u64) -> Option<i64> {
        let Some(rel) = micros.checked_sub(self.start_micros) else {
            self.stats.dropped_early = self.stats.dropped_early.saturating_add(1);
            return None;
        };
        let Some(raw) = micros_to_ticks(rel, self.tempo_bpm, self.ppq) else {
            self.stats.dropped_overflow = self.stats.dropped_overflow.saturating_add(1);
            return None;
        };
        let raw = i64::try_from(raw).unwrap_or(i64::MAX);
        let t = raw.saturating_sub(self.latency_ticks).max(0).max(self.last_tick);
        self.last_tick = t;
        Some(t)
    }

    /// Record one raw message.
    pub fn push_message(&mut self, msg: &TimedMessage) {
        match msg.parse() {
            Some(ev) => self.push_event(msg.micros, &ev),
            None => self.stats.ignored = self.stats.ignored.saturating_add(1),
        }
    }

    /// Record one parsed event stamped `micros` (backend clock).
    pub fn push_event(&mut self, micros: u64, ev: &MidiEvent) {
        let ctrl = match *ev {
            MidiEvent::NoteOn { channel, note, velocity } => {
                self.note_on(micros, channel, note, velocity);
                return;
            }
            MidiEvent::NoteOff { channel, note, velocity } => {
                self.note_off(micros, channel, note, velocity);
                return;
            }
            MidiEvent::ControlChange { channel, controller, value } => (channel, CtrlKind::Cc(controller & 0x7F), i32::from(value & 0x7F)),
            MidiEvent::PitchBend { channel, value } => (channel, CtrlKind::PitchBend, i32::from(value.min(16383)) - 8192),
            MidiEvent::ChannelPressure { channel, pressure } => (channel, CtrlKind::ChannelPressure, i32::from(pressure & 0x7F)),
            MidiEvent::ProgramChange { channel, program } => (channel, CtrlKind::Program, i32::from(program & 0x7F)),
            MidiEvent::Raw(_) => {
                self.stats.ignored = self.stats.ignored.saturating_add(1);
                return;
            }
        };
        if self.stored() >= self.max_events {
            self.stats.dropped_cap = self.stats.dropped_cap.saturating_add(1);
            return;
        }
        if let Some(tick) = self.tick_at(micros) {
            self.ctrls.push(CtrlEvent { tick, channel: ctrl.0 & 0x0F, kind: ctrl.1, value: ctrl.2 });
        }
    }

    fn note_on(&mut self, micros: u64, channel: u8, note: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(micros, channel, note, 0);
            return;
        }
        if self.stored() >= self.max_events {
            self.stats.dropped_cap = self.stats.dropped_cap.saturating_add(1);
            return;
        }
        if let Some(start) = self.tick_at(micros) {
            self.open.entry((channel & 0x0F, note & 0x7F)).or_default().push_back(OpenNote { start, velocity: velocity.clamp(1, 127) });
            self.open_count = self.open_count.saturating_add(1);
        }
    }

    fn note_off(&mut self, micros: u64, channel: u8, note: u8, release: u8) {
        let key = (channel & 0x0F, note & 0x7F);
        // Orphans are ignored, but still validate the time first so early stamps are counted.
        let has_open = self.open.get(&key).is_some_and(|q| !q.is_empty());
        if !has_open {
            self.stats.orphan_offs = self.stats.orphan_offs.saturating_add(1);
            return;
        }
        let Some(end) = self.tick_at(micros) else { return };
        let popped = self.open.get_mut(&key).and_then(VecDeque::pop_front);
        if self.open.get(&key).is_some_and(VecDeque::is_empty) {
            self.open.remove(&key);
        }
        if let Some(o) = popped {
            self.open_count = self.open_count.saturating_sub(1);
            self.notes.push(make_note(key, o, end, release.min(127)));
        }
    }

    /// Stop recording at `stop_micros`: closes still-sounding notes at the stop time (or the
    /// last event time, whichever is later) and returns the sorted [`Sequence`] with stats.
    pub fn finish(mut self, stop_micros: u64) -> (Sequence, TakeStats) {
        let stop = micros_to_ticks(stop_micros.saturating_sub(self.start_micros), self.tempo_bpm, self.ppq)
            .map(|t| i64::try_from(t).unwrap_or(i64::MAX).saturating_sub(self.latency_ticks).max(0))
            .unwrap_or(self.last_tick)
            .max(self.last_tick);
        let open = std::mem::take(&mut self.open);
        let mut stuck = 0usize;
        for (key, q) in open {
            for o in q {
                stuck = stuck.saturating_add(1);
                self.notes.push(make_note(key, o, stop, 64));
            }
        }
        let mut seq = Sequence { notes: self.notes, ctrls: self.ctrls };
        seq.sort();
        let mut stats = self.stats;
        stats.notes = seq.notes.len();
        stats.ctrls = seq.ctrls.len();
        stats.stuck_closed = stuck;
        (seq, stats)
    }
}

fn make_note(key: (u8, u8), o: OpenNote, end: i64, release_velocity: u8) -> Note {
    let length = end.saturating_sub(o.start).max(1);
    Note { pitch: key.1, velocity: o.velocity, release_velocity, channel: key.0, start: o.start, length }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // 120 bpm, ppq 480: 1 tick = 1_000_000 / 960 us. Use ppq 1000 at 60 bpm: 1 tick = 1000 us.
    fn take() -> MidiTake {
        MidiTake::new(1_000, 60.0, 1000, 0, 100).unwrap()
    }
    fn on(t: u64, n: u8, v: u8) -> (u64, MidiEvent) {
        (t, MidiEvent::NoteOn { channel: 0, note: n, velocity: v })
    }
    fn off(t: u64, n: u8) -> (u64, MidiEvent) {
        (t, MidiEvent::NoteOff { channel: 0, note: n, velocity: 40 })
    }
    fn run(t: &mut MidiTake, evs: &[(u64, MidiEvent)]) {
        for (m, e) in evs {
            t.push_event(*m, e);
        }
    }

    #[test]
    fn pairs_basic_note() {
        let mut t = take();
        run(&mut t, &[on(1_000 + 10_000, 60, 100), off(1_000 + 30_000, 60)]);
        let (s, st) = t.finish(1_000 + 40_000);
        assert_eq!(s.notes.len(), 1);
        let n = s.notes[0];
        assert_eq!((n.start, n.length, n.velocity, n.release_velocity, n.pitch), (10, 20, 100, 40, 60));
        assert_eq!(st.stuck_closed, 0);
    }

    #[test]
    fn velocity_zero_note_on_is_off() {
        let mut t = take();
        run(&mut t, &[on(2_000, 60, 90), on(5_000, 60, 0)]);
        let (s, _) = t.finish(9_000);
        assert_eq!(s.notes.len(), 1);
        assert_eq!(s.notes[0].length, 3);
        assert_eq!(s.notes[0].release_velocity, 0);
    }

    #[test]
    fn overlapping_same_pitch_pairs_fifo() {
        let mut t = take();
        run(&mut t, &[on(2_000, 60, 50), on(4_000, 60, 70), off(6_000, 60), off(9_000, 60)]);
        let (s, st) = t.finish(20_000);
        assert_eq!(s.notes.len(), 2);
        assert_eq!((s.notes[0].start, s.notes[0].length, s.notes[0].velocity), (1, 4, 50));
        assert_eq!((s.notes[1].start, s.notes[1].length, s.notes[1].velocity), (3, 5, 70));
        assert_eq!(st.orphan_offs, 0);
    }

    #[test]
    fn stuck_notes_closed_at_stop() {
        let mut t = take();
        run(&mut t, &[on(2_000, 60, 50), on(3_000, 64, 50)]);
        let (s, st) = t.finish(11_000);
        assert_eq!(st.stuck_closed, 2);
        assert!(s.notes.iter().all(|n| n.end() == 10));
    }

    #[test]
    fn stop_before_last_event_still_valid() {
        let mut t = take();
        run(&mut t, &[on(50_000, 60, 50)]);
        let (s, _) = t.finish(0);
        assert_eq!(s.notes.len(), 1);
        assert!(s.notes[0].length >= 1);
    }

    #[test]
    fn orphan_off_ignored() {
        let mut t = take();
        run(&mut t, &[off(3_000, 60)]);
        let (s, st) = t.finish(9_000);
        assert!(s.notes.is_empty());
        assert_eq!(st.orphan_offs, 1);
    }

    #[test]
    fn same_tick_on_off_gets_length_one() {
        let mut t = take();
        run(&mut t, &[on(3_000, 60, 50), off(3_000, 60)]);
        let (s, _) = t.finish(9_000);
        assert_eq!(s.notes[0].length, 1);
    }

    #[test]
    fn collects_controllers() {
        let mut t = take();
        t.push_event(2_000, &MidiEvent::ControlChange { channel: 2, controller: 7, value: 100 });
        t.push_event(3_000, &MidiEvent::PitchBend { channel: 2, value: 0 });
        t.push_event(4_000, &MidiEvent::PitchBend { channel: 2, value: 16383 });
        t.push_event(5_000, &MidiEvent::Raw(vec![0xF8]));
        let (s, st) = t.finish(9_000);
        assert_eq!(s.ctrls.len(), 3);
        assert_eq!(s.ctrls[0].kind, CtrlKind::Cc(7));
        assert_eq!(s.ctrls[1].value, -8192);
        assert_eq!(s.ctrls[2].value, 8191);
        assert_eq!(st.ignored, 1);
    }

    #[test]
    fn push_message_parses_bytes() {
        let mut t = take();
        t.push_message(&TimedMessage::new(2_000, &[0x90, 60, 100]).unwrap());
        t.push_message(&TimedMessage::new(4_000, &[0x80, 60, 0]).unwrap());
        t.push_message(&TimedMessage::new(4_000, &[0x00]).unwrap());
        let (s, st) = t.finish(9_000);
        assert_eq!(s.notes.len(), 1);
        assert_eq!(st.ignored, 1);
    }

    #[test]
    fn flood_is_capped_and_reported() {
        let mut t = MidiTake::new(0, 60.0, 1000, 0, 10).unwrap();
        for i in 0..1000u64 {
            t.push_event(i * 1000, &MidiEvent::ControlChange { channel: 0, controller: 1, value: 5 });
        }
        for i in 0..1000u64 {
            t.push_event(i, &MidiEvent::NoteOn { channel: 0, note: (i % 128) as u8, velocity: 9 });
        }
        let (s, st) = t.finish(2_000_000);
        assert_eq!(s.notes.len() + s.ctrls.len(), 10);
        assert_eq!(st.dropped_cap, 1990);
    }

    #[test]
    fn open_notes_count_toward_cap() {
        let mut t = MidiTake::new(0, 60.0, 1000, 0, 3).unwrap();
        for n in 0..10u8 {
            t.push_event(u64::from(n) * 1000, &MidiEvent::NoteOn { channel: 0, note: n, velocity: 9 });
        }
        let (s, st) = t.finish(20_000);
        assert_eq!(s.notes.len(), 3);
        assert_eq!(st.dropped_cap, 7);
    }

    #[test]
    fn invalid_tempo_and_ppq_rejected() {
        for bpm in [0.0, -120.0, f64::NAN, f64::INFINITY, 1e9] {
            assert_eq!(MidiTake::new(0, bpm, 960, 0, 10).unwrap_err(), RecordError::InvalidTempo);
        }
        assert_eq!(MidiTake::new(0, 120.0, 0, 0, 10).unwrap_err(), RecordError::InvalidPpq);
    }

    #[test]
    fn ppq_extremes() {
        let mut t = MidiTake::new(0, 120.0, u32::MAX, 0, 10).unwrap();
        t.push_event(u64::MAX, &MidiEvent::NoteOn { channel: 0, note: 1, velocity: 1 });
        t.push_event(1_000_000, &MidiEvent::NoteOn { channel: 0, note: 2, velocity: 1 });
        let (s, st) = t.finish(u64::MAX);
        // u64::MAX micros at u32::MAX ppq overflows u64 ticks or saturates to i64; never panics.
        assert!(st.dropped_overflow <= 1);
        assert!(s.notes.iter().all(|n| n.length >= 1));
        let mut t = MidiTake::new(0, 120.0, 1, 0, 10).unwrap();
        t.push_event(1_000_000, &MidiEvent::NoteOn { channel: 0, note: 2, velocity: 1 });
        let (s, _) = t.finish(1_000_000);
        assert_eq!(s.notes[0].start, 2);
    }

    #[test]
    fn latency_offset_saturates() {
        let mut t = MidiTake::new(0, 60.0, 1000, 5, 10).unwrap();
        t.push_event(3_000, &MidiEvent::NoteOn { channel: 0, note: 1, velocity: 1 });
        t.push_event(20_000, &MidiEvent::NoteOff { channel: 0, note: 1, velocity: 0 });
        let (s, _) = t.finish(30_000);
        assert_eq!(s.notes[0].start, 0);
        assert_eq!(s.notes[0].end(), 15);
        let mut t = MidiTake::new(0, 60.0, 1000, i64::MIN, 10).unwrap();
        t.push_event(3_000, &MidiEvent::NoteOn { channel: 0, note: 1, velocity: 1 });
        let (s, _) = t.finish(30_000);
        assert_eq!(s.notes.len(), 1);
        let mut t = MidiTake::new(0, 60.0, 1000, i64::MAX, 10).unwrap();
        t.push_event(3_000, &MidiEvent::NoteOn { channel: 0, note: 1, velocity: 1 });
        let (s, _) = t.finish(30_000);
        assert_eq!(s.notes[0].start, 0);
    }

    #[test]
    fn early_events_dropped() {
        let mut t = take();
        run(&mut t, &[on(500, 60, 50)]);
        let (s, st) = t.finish(9_000);
        assert!(s.notes.is_empty());
        assert_eq!(st.dropped_early, 1);
    }

    #[test]
    fn backwards_timestamps_stay_monotonic() {
        let mut t = take();
        run(&mut t, &[on(9_000, 60, 50), on(3_000, 62, 50), off(4_000, 62), off(12_000, 60)]);
        let (s, _) = t.finish(20_000);
        let n62 = s.notes.iter().find(|n| n.pitch == 62).unwrap();
        assert_eq!(n62.start, 8);
        assert!(s.notes.iter().all(|n| n.length >= 1));
    }

    #[test]
    fn output_sorted() {
        let mut t = take();
        run(&mut t, &[on(2_000, 70, 5), on(2_000, 60, 5), off(4_000, 70), off(4_000, 60)]);
        t.push_event(3_000, &MidiEvent::ControlChange { channel: 0, controller: 1, value: 1 });
        let (s, _) = t.finish(9_000);
        assert!(s.notes.windows(2).all(|w| (w[0].start, w[0].pitch) <= (w[1].start, w[1].pitch)));
        assert!(s.ctrls.windows(2).all(|w| w[0].tick <= w[1].tick));
    }

    fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            (0u8..16, 0u8..128, 0u8..128).prop_map(|(c, a, b)| vec![0x90 | c, a, b]),
            (0u8..16, 0u8..128, 0u8..128).prop_map(|(c, a, b)| vec![0x80 | c, a, b]),
            (0u8..16, 0u8..128, 0u8..128).prop_map(|(c, a, b)| vec![0xB0 | c, a, b]),
            (0u8..16, 0u8..128, 0u8..128).prop_map(|(c, a, b)| vec![0xE0 | c, a, b]),
            proptest::collection::vec(any::<u8>(), 1..8),
        ]
    }

    proptest! {
        #[test]
        fn never_panics_and_notes_valid(
            msgs in proptest::collection::vec((any::<u64>(), arb_bytes()), 0..200),
            bpm in prop_oneof![Just(120.0f64), -10.0f64..20000.0, Just(f64::NAN)],
            ppq in prop_oneof![Just(960u32), any::<u32>()],
            latency in any::<i64>(),
            cap in 0usize..50,
            start in any::<u64>(),
            stop in any::<u64>(),
        ) {
            if let Ok(mut t) = MidiTake::new(start, bpm, ppq, latency, cap) {
                for (m, b) in &msgs {
                    if let Some(tm) = TimedMessage::new(*m, b) {
                        t.push_message(&tm);
                    }
                }
                let (s, st) = t.finish(stop);
                prop_assert!(s.notes.len() + s.ctrls.len() <= cap.max(1));
                prop_assert_eq!(st.notes, s.notes.len());
                for n in &s.notes {
                    prop_assert!(n.end() >= n.start);
                    prop_assert!(n.length >= 1 && n.start >= 0);
                    prop_assert!((1..=127).contains(&n.velocity));
                }
                prop_assert!(s.ctrls.windows(2).all(|w| w[0].tick <= w[1].tick));
            }
        }

        #[test]
        fn small_clock_streams_monotonic(
            msgs in proptest::collection::vec((0u64..5_000_000, arb_bytes()), 0..200),
        ) {
            let mut t = MidiTake::new(0, 120.0, 960, 0, 10_000).unwrap();
            for (m, b) in &msgs {
                if let Some(tm) = TimedMessage::new(*m, b) {
                    t.push_message(&tm);
                }
            }
            let (s, _) = t.finish(5_000_000);
            prop_assert!(s.notes.iter().all(|n| n.end() >= n.start));
            prop_assert!(s.notes.windows(2).all(|w| (w[0].start, w[0].pitch) <= (w[1].start, w[1].pitch)));
        }
    }
}

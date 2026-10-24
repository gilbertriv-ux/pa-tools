//! Hardware MIDI ports: enumeration, live input and output (G-02), through the `midir` crate.
//!
//! The parts that need real hardware ([`list_inputs`], [`open_input`], [`open_output`]) are thin
//! wrappers; everything else is pure and unit-tested: [`parse_midi_message`],
//! [`micros_to_ticks`], [`select_port`] and the callback-side [`forward_message`].
//!
//! A missing MIDI backend (for example no ALSA sequencer in a container) is reported as an empty
//! port list by [`list_inputs`]/[`list_outputs`] and as [`MidiIoError::Backend`] by the open
//! functions; it never panics. The input callback never blocks: messages go through `try_send`
//! into a bounded channel and are counted in [`MidiInput::dropped`] when the queue is full.
//!
//! Note: this module does not reuse `soundcraft-midi` types because `soundcraft-playback` does not
//! depend on that crate; [`MidiEvent`] is a small wire-level type for the caller to map.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

/// Longest message accepted (parser, output and the inline input buffer), in bytes.
pub const MAX_MESSAGE_LEN: usize = 64;
/// Largest input queue capacity accepted by [`open_input`].
pub const MAX_QUEUE: usize = 65_536;
/// Largest tempo accepted by [`micros_to_ticks`].
pub const MAX_TEMPO_BPM: f64 = 10_000.0;

const CLIENT_NAME: &str = "Pa Tools";

/// A MIDI port as listed by the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortInfo {
    pub index: usize,
    pub name: String,
}

/// Errors from MIDI port handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiIoError {
    /// The MIDI backend is not available (no sequencer, no permission, unsupported platform).
    Backend(String),
    /// No port matches the given name or index.
    PortNotFound(String),
    /// Queue capacity is zero or above [`MAX_QUEUE`].
    InvalidQueue(usize),
    /// Malformed, empty or oversized MIDI message.
    InvalidMessage,
    /// Connecting to the port failed.
    Connect(String),
    /// Sending failed.
    Send(String),
}

impl fmt::Display for MidiIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(m) => write!(f, "MIDI backend unavailable: {m}"),
            Self::PortNotFound(p) => write!(f, "MIDI port not found: {p}"),
            Self::InvalidQueue(n) => write!(f, "invalid MIDI queue size {n} (1..={MAX_QUEUE})"),
            Self::InvalidMessage => write!(f, "invalid MIDI message"),
            Self::Connect(m) => write!(f, "MIDI connect failed: {m}"),
            Self::Send(m) => write!(f, "MIDI send failed: {m}"),
        }
    }
}

impl std::error::Error for MidiIoError {}

/// A parsed MIDI message. Channels are 0..=15; data values are 0..=127 (pitch bend is 0..=16383,
/// centre 8192).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        note: u8,
        velocity: u8,
    },
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    PitchBend {
        channel: u8,
        value: u16,
    },
    ProgramChange {
        channel: u8,
        program: u8,
    },
    ChannelPressure {
        channel: u8,
        pressure: u8,
    },
    /// Any other well-formed message (polyphonic pressure, system common/realtime, SysEx).
    Raw(Vec<u8>),
}

/// Parse one complete MIDI message. Returns `None` for empty, short, long, oversized or
/// malformed input (missing status byte, data byte above 127, bad SysEx framing).
/// A note-on with velocity 0 is returned as `NoteOff` (velocity 0).
pub fn parse_midi_message(bytes: &[u8]) -> Option<MidiEvent> {
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_LEN {
        return None;
    }
    let status = *bytes.first()?;
    if status < 0x80 {
        return None;
    }
    let data = bytes.get(1..)?;
    if status >= 0xF0 {
        let want = match status {
            0xF0 => {
                // SysEx: data bytes then a terminating 0xF7.
                let (last, body) = data.split_last()?;
                return (*last == 0xF7 && body.iter().all(|b| *b < 0x80)).then(|| MidiEvent::Raw(bytes.to_vec()));
            }
            0xF1 | 0xF3 => 1,
            0xF2 => 2,
            0xF6 | 0xF8 | 0xFA | 0xFB | 0xFC | 0xFE | 0xFF => 0,
            _ => return None, // 0xF4, 0xF5, 0xF7, 0xF9, 0xFD: undefined or stray
        };
        return (data.len() == want && data.iter().all(|b| *b < 0x80)).then(|| MidiEvent::Raw(bytes.to_vec()));
    }
    let channel = status & 0x0F;
    let want = match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        _ => 2,
    };
    if data.len() != want || data.iter().any(|b| *b >= 0x80) {
        return None;
    }
    let d0 = *data.first()?;
    let d1 = data.get(1).copied().unwrap_or(0);
    Some(match status & 0xF0 {
        0x80 => MidiEvent::NoteOff { channel, note: d0, velocity: d1 },
        0x90 if d1 == 0 => MidiEvent::NoteOff { channel, note: d0, velocity: 0 },
        0x90 => MidiEvent::NoteOn { channel, note: d0, velocity: d1 },
        0xB0 => MidiEvent::ControlChange { channel, controller: d0, value: d1 },
        0xC0 => MidiEvent::ProgramChange { channel, program: d0 },
        0xD0 => MidiEvent::ChannelPressure { channel, pressure: d0 },
        0xE0 => MidiEvent::PitchBend { channel, value: u16::from(d0) | (u16::from(d1) << 7) },
        _ => MidiEvent::Raw(bytes.to_vec()), // 0xA0 polyphonic pressure
    })
}

/// Convert a time in microseconds to ticks at a constant tempo and resolution (rounded down).
/// `None` when the tempo is not finite, `<= 0` or above [`MAX_TEMPO_BPM`], `ppq` is 0, or the
/// result does not fit in `u64`.
pub fn micros_to_ticks(micros: u64, tempo_bpm: f64, ppq: u32) -> Option<u64> {
    if !tempo_bpm.is_finite() || tempo_bpm <= 0.0 || tempo_bpm > MAX_TEMPO_BPM || ppq == 0 {
        return None;
    }
    // Milli-BPM, at least 1 so very slow tempos do not collapse to zero.
    let milli_bpm = ((tempo_bpm * 1000.0).round() as u64).max(1);
    // ticks = micros * (bpm / 60) * ppq / 1e6 = micros * milli_bpm * ppq / 6e10
    let num = u128::from(micros).checked_mul(u128::from(milli_bpm))?.checked_mul(u128::from(ppq))?;
    u64::try_from(num / 60_000_000_000u128).ok()
}

/// Pick a port from `names` by index ("2"), exact name, or unique-first case-insensitive
/// substring, in that order. Returns the port index.
pub fn select_port(names: &[String], name_or_index: &str) -> Option<usize> {
    let key = name_or_index.trim();
    if key.is_empty() {
        return None;
    }
    if key.bytes().all(|b| b.is_ascii_digit()) {
        // All digits: an index, or out of range (also when too large to parse).
        return key.parse::<usize>().ok().filter(|i| *i < names.len());
    }
    if let Some(i) = names.iter().position(|n| n == key) {
        return Some(i);
    }
    let lower = key.to_lowercase();
    names.iter().position(|n| n.to_lowercase().contains(&lower))
}

/// A raw message with its backend timestamp, stored inline so the callback does not allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedMessage {
    /// Microseconds, relative to an arbitrary backend epoch.
    pub micros: u64,
    len: u8,
    data: [u8; MAX_MESSAGE_LEN],
}

impl TimedMessage {
    /// Build from raw bytes; `None` if empty or longer than [`MAX_MESSAGE_LEN`].
    pub fn new(micros: u64, bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_MESSAGE_LEN {
            return None;
        }
        let mut data = [0u8; MAX_MESSAGE_LEN];
        data.get_mut(..bytes.len())?.copy_from_slice(bytes);
        Some(Self { micros, len: u8::try_from(bytes.len()).ok()?, data })
    }

    /// The message bytes.
    pub fn bytes(&self) -> &[u8] {
        self.data.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Parse with [`parse_midi_message`].
    pub fn parse(&self) -> Option<MidiEvent> {
        parse_midi_message(self.bytes())
    }
}

/// Callback-side forwarder: never blocks. Counts a drop when the queue is full, disconnected or
/// the message is oversized/empty.
pub fn forward_message(tx: &SyncSender<TimedMessage>, dropped: &AtomicU64, micros: u64, bytes: &[u8]) {
    let sent = match TimedMessage::new(micros, bytes) {
        Some(m) => match tx.try_send(m) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        },
        None => false,
    };
    if !sent {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn backend_err(e: impl fmt::Display) -> MidiIoError {
    MidiIoError::Backend(e.to_string())
}

/// List MIDI input ports. An unavailable backend yields `Ok(vec![])`.
pub fn list_inputs() -> Result<Vec<MidiPortInfo>, MidiIoError> {
    let Ok(m) = midir::MidiInput::new(CLIENT_NAME) else {
        return Ok(Vec::new());
    };
    Ok(m.ports().iter().enumerate().map(|(index, p)| MidiPortInfo { index, name: m.port_name(p).unwrap_or_default() }).collect())
}

/// List MIDI output ports. An unavailable backend yields `Ok(vec![])`.
pub fn list_outputs() -> Result<Vec<MidiPortInfo>, MidiIoError> {
    let Ok(m) = midir::MidiOutput::new(CLIENT_NAME) else {
        return Ok(Vec::new());
    };
    Ok(m.ports().iter().enumerate().map(|(index, p)| MidiPortInfo { index, name: m.port_name(p).unwrap_or_default() }).collect())
}

/// Pairs the backend clock (whose epoch differs per platform) with the local monotonic clock,
/// using the first message seen: written once by the input callback, never blocking.
#[derive(Debug)]
pub struct ClockAnchor {
    epoch: Instant,
    /// Backend microseconds of the first message (valid once `elapsed_us` is set).
    backend_us: AtomicU64,
    /// Microseconds since `epoch` when the first message arrived; [`Self::UNSET`] before that.
    elapsed_us: AtomicU64,
}

impl ClockAnchor {
    const UNSET: u64 = u64::MAX;

    /// A new anchor whose local clock starts at `epoch`.
    pub fn new(epoch: Instant) -> Self {
        Self { epoch, backend_us: AtomicU64::new(0), elapsed_us: AtomicU64::new(Self::UNSET) }
    }

    /// Callback side: remember the first message's pair. Later calls change nothing.
    pub fn observe(&self, backend_micros: u64) {
        if self.elapsed_us.load(Ordering::Acquire) != Self::UNSET {
            return;
        }
        let elapsed = u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(Self::UNSET - 1).min(Self::UNSET - 1);
        self.backend_us.store(backend_micros, Ordering::Relaxed);
        self.elapsed_us.store(elapsed, Ordering::Release);
    }

    /// The backend timestamp that corresponds to the local instant `at`, or `None` until a
    /// message has been seen. Saturates at 0 for instants before the backend epoch.
    pub fn micros_at(&self, at: Instant) -> Option<u64> {
        let seen = self.elapsed_us.load(Ordering::Acquire);
        if seen == Self::UNSET {
            return None;
        }
        let at_us = i128::try_from(at.saturating_duration_since(self.epoch).as_micros()).ok()?;
        Some(anchor_to_backend(self.backend_us.load(Ordering::Relaxed), seen, at_us))
    }
}

/// Map a local time `at_us` (microseconds since the anchor's epoch) to the backend clock, given
/// that backend time `backend_us` was seen at local time `seen_us`. Saturating and never negative.
pub fn anchor_to_backend(backend_us: u64, seen_us: u64, at_us: i128) -> u64 {
    let v = i128::from(backend_us).saturating_add(at_us).saturating_sub(i128::from(seen_us));
    u64::try_from(v.max(0)).unwrap_or(u64::MAX)
}

/// An open hardware MIDI input. Dropping it disconnects.
pub struct MidiInput {
    conn: Option<midir::MidiInputConnection<()>>,
    dropped: Arc<AtomicU64>,
    port_name: String,
    anchor: Arc<ClockAnchor>,
}

impl MidiInput {
    /// Messages dropped because the queue was full or the message was unusable.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Name of the connected port.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// The backend timestamp matching the local instant `at` (see [`ClockAnchor`]); `None` until
    /// the first message has arrived.
    pub fn micros_at(&self, at: Instant) -> Option<u64> {
        self.anchor.micros_at(at)
    }

    /// Disconnect explicitly.
    pub fn close(&mut self) {
        if let Some(c) = self.conn.take() {
            let _ = c.close();
        }
    }
}

/// Open an input port by index ("0"), exact name or name fragment. Timestamped raw messages
/// arrive on the returned receiver, a bounded queue of `queue` messages (1..=[`MAX_QUEUE`]).
pub fn open_input(name_or_index: &str, queue: usize) -> Result<(MidiInput, Receiver<TimedMessage>), MidiIoError> {
    if queue == 0 || queue > MAX_QUEUE {
        return Err(MidiIoError::InvalidQueue(queue));
    }
    let backend = midir::MidiInput::new(CLIENT_NAME).map_err(backend_err)?;
    let ports = backend.ports();
    let names: Vec<String> = ports.iter().map(|p| backend.port_name(p).unwrap_or_default()).collect();
    let idx = select_port(&names, name_or_index).ok_or_else(|| MidiIoError::PortNotFound(name_or_index.to_string()))?;
    let port = ports.get(idx).ok_or_else(|| MidiIoError::PortNotFound(name_or_index.to_string()))?;
    let port_name = names.get(idx).cloned().unwrap_or_default();
    let (tx, rx) = sync_channel(queue);
    let dropped = Arc::new(AtomicU64::new(0));
    let cb_dropped = Arc::clone(&dropped);
    let anchor = Arc::new(ClockAnchor::new(Instant::now()));
    let cb_anchor = Arc::clone(&anchor);
    let conn = backend
        .connect(
            port,
            "pa-tools-in",
            move |micros, bytes, _| {
                cb_anchor.observe(micros);
                forward_message(&tx, &cb_dropped, micros, bytes);
            },
            (),
        )
        .map_err(|e| MidiIoError::Connect(e.to_string()))?;
    Ok((MidiInput { conn: Some(conn), dropped, port_name, anchor }, rx))
}

/// An open hardware MIDI output. Dropping it disconnects.
pub struct MidiOutput {
    conn: midir::MidiOutputConnection,
    port_name: String,
}

impl MidiOutput {
    /// Name of the connected port.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Send one complete, well-formed message (validated with [`parse_midi_message`]).
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), MidiIoError> {
        if parse_midi_message(bytes).is_none() {
            return Err(MidiIoError::InvalidMessage);
        }
        self.conn.send(bytes).map_err(|e| MidiIoError::Send(e.to_string()))
    }
}

/// Open an output port by index ("0"), exact name or name fragment.
pub fn open_output(name_or_index: &str) -> Result<MidiOutput, MidiIoError> {
    let backend = midir::MidiOutput::new(CLIENT_NAME).map_err(backend_err)?;
    let ports = backend.ports();
    let names: Vec<String> = ports.iter().map(|p| backend.port_name(p).unwrap_or_default()).collect();
    let idx = select_port(&names, name_or_index).ok_or_else(|| MidiIoError::PortNotFound(name_or_index.to_string()))?;
    let port = ports.get(idx).ok_or_else(|| MidiIoError::PortNotFound(name_or_index.to_string()))?;
    let port_name = names.get(idx).cloned().unwrap_or_default();
    let conn = backend.connect(port, "pa-tools-out").map_err(|e| MidiIoError::Connect(e.to_string()))?;
    Ok(MidiOutput { conn, port_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_maps_local_time_to_the_backend_clock() {
        assert_eq!(anchor_to_backend(5_000_000, 1_000, 3_000), 5_002_000);
        assert_eq!(anchor_to_backend(5_000_000, 1_000, 0), 4_999_000);
        assert_eq!(anchor_to_backend(10, 1_000, 0), 0, "before the backend epoch saturates at 0");
        assert_eq!(anchor_to_backend(u64::MAX, 0, i128::MAX), u64::MAX);
    }

    #[test]
    fn anchor_is_set_once_by_the_first_message() {
        let t0 = Instant::now();
        let a = ClockAnchor::new(t0);
        assert_eq!(a.micros_at(t0), None, "no message yet");
        a.observe(7_000_000);
        let first = a.micros_at(t0 + std::time::Duration::from_secs(1));
        a.observe(99_000_000);
        assert_eq!(a.micros_at(t0 + std::time::Duration::from_secs(1)), first, "later messages do not move it");
        let (one, two) = (a.micros_at(t0 + std::time::Duration::from_secs(1)), a.micros_at(t0 + std::time::Duration::from_secs(2)));
        assert_eq!(two.zip(one).map(|(b, a)| b - a), Some(1_000_000));
    }

    #[test]
    fn parses_channel_messages() {
        assert_eq!(parse_midi_message(&[0x93, 60, 100]), Some(MidiEvent::NoteOn { channel: 3, note: 60, velocity: 100 }));
        assert_eq!(parse_midi_message(&[0x93, 60, 0]), Some(MidiEvent::NoteOff { channel: 3, note: 60, velocity: 0 }));
        assert_eq!(parse_midi_message(&[0x80, 60, 40]), Some(MidiEvent::NoteOff { channel: 0, note: 60, velocity: 40 }));
        assert_eq!(parse_midi_message(&[0xB1, 7, 99]), Some(MidiEvent::ControlChange { channel: 1, controller: 7, value: 99 }));
        assert_eq!(parse_midi_message(&[0xC2, 5]), Some(MidiEvent::ProgramChange { channel: 2, program: 5 }));
        assert_eq!(parse_midi_message(&[0xD2, 9]), Some(MidiEvent::ChannelPressure { channel: 2, pressure: 9 }));
        assert_eq!(parse_midi_message(&[0xE0, 0x00, 0x40]), Some(MidiEvent::PitchBend { channel: 0, value: 8192 }));
        assert_eq!(parse_midi_message(&[0xEF, 0x7F, 0x7F]), Some(MidiEvent::PitchBend { channel: 15, value: 16383 }));
    }

    #[test]
    fn parses_other_as_raw() {
        assert_eq!(parse_midi_message(&[0xA0, 60, 10]), Some(MidiEvent::Raw(vec![0xA0, 60, 10])));
        assert_eq!(parse_midi_message(&[0xF8]), Some(MidiEvent::Raw(vec![0xF8])));
        assert_eq!(parse_midi_message(&[0xF2, 1, 2]), Some(MidiEvent::Raw(vec![0xF2, 1, 2])));
        assert_eq!(parse_midi_message(&[0xF0, 1, 2, 0xF7]), Some(MidiEvent::Raw(vec![0xF0, 1, 2, 0xF7])));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_midi_message(&[]), None);
        assert_eq!(parse_midi_message(&[60, 100]), None); // no status
        assert_eq!(parse_midi_message(&[0x90, 60]), None); // short
        assert_eq!(parse_midi_message(&[0x90, 60, 100, 1]), None); // long
        assert_eq!(parse_midi_message(&[0x90, 200, 100]), None); // data byte high
        assert_eq!(parse_midi_message(&[0xC0]), None);
        assert_eq!(parse_midi_message(&[0xC0, 1, 2]), None);
        assert_eq!(parse_midi_message(&[0xF0, 1, 2]), None); // unterminated sysex
        assert_eq!(parse_midi_message(&[0xF0, 0x90, 0xF7]), None); // status inside sysex
        assert_eq!(parse_midi_message(&[0xF4]), None);
        assert_eq!(parse_midi_message(&[0xF7]), None);
        assert_eq!(parse_midi_message(&[0xF8, 0]), None);
        let mut big = vec![0xF0];
        big.extend(std::iter::repeat_n(1u8, MAX_MESSAGE_LEN));
        big.push(0xF7);
        assert_eq!(parse_midi_message(&big), None);
    }

    #[test]
    fn parser_survives_every_short_input() {
        for a in 0..=255u8 {
            let _ = parse_midi_message(&[a]);
            for b in [0u8, 1, 127, 128, 255] {
                let _ = parse_midi_message(&[a, b]);
                let _ = parse_midi_message(&[a, b, b]);
            }
        }
    }

    #[test]
    fn ticks_conversion() {
        // 120 bpm, 480 ppq: one beat = 500_000 us = 480 ticks.
        assert_eq!(micros_to_ticks(500_000, 120.0, 480), Some(480));
        assert_eq!(micros_to_ticks(0, 120.0, 480), Some(0));
        assert_eq!(micros_to_ticks(1_000_000, 60.0, 96), Some(96));
        assert_eq!(micros_to_ticks(1_000_000, 90.5, 960), Some(1_448));
    }

    #[test]
    fn ticks_rejects_bad_input() {
        assert_eq!(micros_to_ticks(1, 0.0, 480), None);
        assert_eq!(micros_to_ticks(1, -5.0, 480), None);
        assert_eq!(micros_to_ticks(1, f64::NAN, 480), None);
        assert_eq!(micros_to_ticks(1, f64::INFINITY, 480), None);
        assert_eq!(micros_to_ticks(1, MAX_TEMPO_BPM + 1.0, 480), None);
        assert_eq!(micros_to_ticks(1, 120.0, 0), None);
        // max inputs do not panic and report overflow as None.
        assert_eq!(micros_to_ticks(u64::MAX, MAX_TEMPO_BPM, u32::MAX), None);
        assert!(micros_to_ticks(u64::MAX, 0.001, 1).is_some());
    }

    #[test]
    fn port_selection() {
        let names: Vec<String> = ["Keys Mini", "Drum Pad", "Keys Mini 2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(select_port(&names, "1"), Some(1));
        assert_eq!(select_port(&names, "3"), None);
        assert_eq!(select_port(&names, "99999999999999999999999"), None);
        assert_eq!(select_port(&names, "Keys Mini 2"), Some(2));
        assert_eq!(select_port(&names, "drum"), Some(1));
        assert_eq!(select_port(&names, "keys"), Some(0));
        assert_eq!(select_port(&names, ""), None);
        assert_eq!(select_port(&names, "nope"), None);
        assert_eq!(select_port(&[], "0"), None);
    }

    #[test]
    fn forward_never_blocks_and_counts_drops() {
        let (tx, rx) = sync_channel(2);
        let dropped = AtomicU64::new(0);
        for i in 0..5u64 {
            forward_message(&tx, &dropped, i, &[0x90, 60, 100]);
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        let first = rx.try_recv().expect("queued");
        assert_eq!(first.micros, 0);
        assert_eq!(first.bytes(), &[0x90, 60, 100]);
        assert_eq!(first.parse(), Some(MidiEvent::NoteOn { channel: 0, note: 60, velocity: 100 }));
        // empty and oversized count as drops
        forward_message(&tx, &dropped, 9, &[]);
        forward_message(&tx, &dropped, 9, &[0u8; MAX_MESSAGE_LEN + 1]);
        assert_eq!(dropped.load(Ordering::Relaxed), 5);
        // disconnected receiver counts as a drop
        drop(rx);
        forward_message(&tx, &dropped, 1, &[0xF8]);
        assert_eq!(dropped.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn open_input_rejects_bad_queue() {
        assert_eq!(open_input("0", 0).err(), Some(MidiIoError::InvalidQueue(0)));
        assert_eq!(open_input("0", MAX_QUEUE + 1).err(), Some(MidiIoError::InvalidQueue(MAX_QUEUE + 1)));
    }

    #[test]
    fn listing_and_opening_without_hardware_does_not_panic() {
        // In a container there is usually no MIDI backend or no ports; either way: no panic.
        let ins = list_inputs();
        assert!(ins.is_ok());
        let _ = list_outputs();
        if ins.map(|v| v.is_empty()).unwrap_or(true) {
            assert!(open_input("0", 8).is_err());
            assert!(open_output("0").is_err());
        }
    }

    #[test]
    fn timed_message_roundtrip() {
        assert!(TimedMessage::new(0, &[]).is_none());
        let full = [1u8; MAX_MESSAGE_LEN];
        assert_eq!(TimedMessage::new(7, &full).map(|m| m.bytes().len()), Some(MAX_MESSAGE_LEN));
    }
}

//! Safe MIDI device contracts and bounded short-message transport.
//!
//! This foundation does not enumerate hardware or manufacture devices. Platform adapters may
//! implement [`MidiSystem`] using safe wrappers without changing the workspace unsafe-code gate.
//! Enumeration, connection setup, SysEx allocation, and output sends belong on a MIDI/control
//! worker, never the audio callback. Short-message queue operations allocate and lock nothing.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Maximum complete SysEx packet, including F0/F7. Allocation happens off the audio thread.
pub const MAX_SYSEX_BYTES: usize = 65_536;
/// Upper bound on queued short messages; memory is allocated once at setup.
pub const MAX_QUEUE_MESSAGES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiIoError {
    InvalidMessage(&'static str),
    InvalidPort,
    InvalidCapacity,
    Unsupported,
    Disconnected,
    QueueFull,
    Backend(String),
}

impl fmt::Display for MidiIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMessage(reason) => write!(f, "invalid MIDI message: {reason}"),
            Self::InvalidPort => f.write_str("invalid MIDI port identifier"),
            Self::InvalidCapacity => f.write_str("MIDI queue capacity must be a power of two in 2..=4096"),
            Self::Unsupported => f.write_str("hardware MIDI backend is not enabled"),
            Self::Disconnected => f.write_str("MIDI device disconnected"),
            Self::QueueFull => f.write_str("MIDI input queue is full"),
            Self::Backend(reason) => write!(f, "MIDI backend: {reason}"),
        }
    }
}
impl std::error::Error for MidiIoError {}

/// A complete MIDI 1.0 short message. Running status and undefined status bytes are rejected.
/// Note-on with zero velocity is retained verbatim; interpretation belongs to the event layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortMessage {
    bytes: [u8; 3],
    len: u8,
}
impl ShortMessage {
    pub fn new(bytes: &[u8]) -> Result<Self, MidiIoError> {
        let status = bytes.first().copied().ok_or(MidiIoError::InvalidMessage("empty packet"))?;
        let len = match status {
            0x80..=0xbf | 0xe0..=0xef | 0xf2 => 3,
            0xc0..=0xdf | 0xf1 | 0xf3 => 2,
            0xf6 | 0xf8 | 0xfa..=0xfc | 0xfe..=0xff => 1,
            _ => return Err(MidiIoError::InvalidMessage("unsupported status")),
        };
        if bytes.len() != usize::from(len) || bytes.iter().skip(1).any(|b| *b >= 0x80) {
            return Err(MidiIoError::InvalidMessage("incorrect length or non-data byte"));
        }
        let mut data = [0; 3];
        for (dst, src) in data.iter_mut().zip(bytes) {
            *dst = *src;
        }
        Ok(Self { bytes: data, len })
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// Validated complete SysEx, capped before allocation. Embedded realtime events must be
/// separated by a wire-stream decoder before constructing this packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysEx(Vec<u8>);
impl SysEx {
    pub fn new(bytes: &[u8]) -> Result<Self, MidiIoError> {
        if !(2..=MAX_SYSEX_BYTES).contains(&bytes.len()) || bytes.first() != Some(&0xf0) || bytes.last() != Some(&0xf7) {
            return Err(MidiIoError::InvalidMessage("invalid SysEx envelope or size"));
        }
        let body = bytes.get(1..bytes.len().saturating_sub(1)).ok_or(MidiIoError::InvalidMessage("invalid SysEx body"))?;
        if body.iter().any(|b| *b >= 0x80) {
            return Err(MidiIoError::InvalidMessage("non-data byte in SysEx"));
        }
        Ok(Self(bytes.to_vec()))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Backend-assigned identity, not a device name or enumeration index. Adapters must reject
/// stale IDs after refresh/disconnection rather than opening a different port at the same index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MidiPortId(String);
impl MidiPortId {
    pub fn new(id: String) -> Result<Self, MidiIoError> {
        if id.is_empty() || id.len() > 1024 {
            return Err(MidiIoError::InvalidPort);
        }
        Ok(Self(id))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiInputPort {
    pub id: MidiPortId,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiOutputPort {
    pub id: MidiPortId,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedShortMessage {
    /// Connection-local microseconds, not an audio sample clock or a cross-device epoch.
    pub timestamp_us: u64,
    pub message: ShortMessage,
}

/// Nonblocking short-message receiver. SysEx requires a separate bounded control-worker path.
pub trait MidiInputConnection: Send {
    fn try_receive(&mut self) -> Result<Option<TimedShortMessage>, MidiIoError>;
    fn overflow_count(&self) -> usize;
}
pub trait MidiOutputConnection: Send {
    /// Run on the MIDI worker; safe API does not imply realtime-safe OS output.
    fn send(&mut self, message: ShortMessage) -> Result<(), MidiIoError>;
    fn send_sysex(&mut self, _message: &SysEx) -> Result<(), MidiIoError> {
        Err(MidiIoError::Unsupported)
    }
}
pub trait MidiSystem {
    fn inputs(&mut self) -> Result<Vec<MidiInputPort>, MidiIoError>;
    fn outputs(&mut self) -> Result<Vec<MidiOutputPort>, MidiIoError>;
    fn open_input(&mut self, port: &MidiInputPort) -> Result<Box<dyn MidiInputConnection>, MidiIoError>;
    fn open_output(&mut self, port: &MidiOutputPort) -> Result<Box<dyn MidiOutputConnection>, MidiIoError>;
}

/// Honest default until a platform adapter is explicitly enabled. A working backend with no
/// attached devices should instead return `Ok(Vec::new())` from its enumeration methods.
#[derive(Default)]
pub struct UnsupportedMidiSystem;
impl MidiSystem for UnsupportedMidiSystem {
    fn inputs(&mut self) -> Result<Vec<MidiInputPort>, MidiIoError> {
        Err(MidiIoError::Unsupported)
    }
    fn outputs(&mut self) -> Result<Vec<MidiOutputPort>, MidiIoError> {
        Err(MidiIoError::Unsupported)
    }
    fn open_input(&mut self, _: &MidiInputPort) -> Result<Box<dyn MidiInputConnection>, MidiIoError> {
        Err(MidiIoError::Unsupported)
    }
    fn open_output(&mut self, _: &MidiOutputPort) -> Result<Box<dyn MidiOutputConnection>, MidiIoError> {
        Err(MidiIoError::Unsupported)
    }
}

struct Slot {
    bytes: AtomicU32,
    timestamp: AtomicU64,
}
struct Queue {
    slots: Box<[Slot]>,
    head: AtomicUsize,
    tail: AtomicUsize,
    overflow: AtomicUsize,
}
/// Unique producer endpoint; `&mut self` serializes access. This endpoint cannot be cloned.
pub struct MidiInputSender(Arc<Queue>);
/// Unique consumer endpoint; `&mut self` serializes access. This endpoint cannot be cloned.
pub struct MidiInputReceiver(Arc<Queue>);

pub fn input_queue(capacity: usize) -> Result<(MidiInputSender, MidiInputReceiver), MidiIoError> {
    if !(2..=MAX_QUEUE_MESSAGES).contains(&capacity) || !capacity.is_power_of_two() {
        return Err(MidiIoError::InvalidCapacity);
    }
    let queue = Arc::new(Queue {
        slots: (0..capacity).map(|_| Slot { bytes: AtomicU32::new(0), timestamp: AtomicU64::new(0) }).collect(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        overflow: AtomicUsize::new(0),
    });
    Ok((MidiInputSender(Arc::clone(&queue)), MidiInputReceiver(queue)))
}
impl MidiInputSender {
    /// Reject newest on overflow, preserving the order and timestamp of queued messages.
    pub fn try_send(&mut self, event: TimedShortMessage) -> Result<(), MidiIoError> {
        let q = &self.0;
        let head = q.head.load(Ordering::Relaxed);
        if head.wrapping_sub(q.tail.load(Ordering::Acquire)) >= q.slots.len() {
            let _ = q.overflow.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(1)));
            return Err(MidiIoError::QueueFull);
        }
        let slot = q.slots.get(head % q.slots.len()).ok_or(MidiIoError::InvalidCapacity)?;
        let [a, b, c] = event.message.bytes;
        slot.bytes.store(u32::from_le_bytes([a, b, c, event.message.len]), Ordering::Relaxed);
        slot.timestamp.store(event.timestamp_us, Ordering::Relaxed);
        q.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}
impl MidiInputConnection for MidiInputReceiver {
    fn try_receive(&mut self) -> Result<Option<TimedShortMessage>, MidiIoError> {
        let q = &self.0;
        let tail = q.tail.load(Ordering::Relaxed);
        if tail == q.head.load(Ordering::Acquire) {
            if Arc::strong_count(q) != 1 {
                return Ok(None);
            }
            // The producer may have published its final message before dropping its Arc.
            // Once its endpoint is gone, acquire the final head before declaring disconnect.
            if tail == q.head.load(Ordering::Acquire) {
                return Err(MidiIoError::Disconnected);
            }
        }
        let slot = q.slots.get(tail % q.slots.len()).ok_or(MidiIoError::InvalidCapacity)?;
        let [a, b, c, len] = slot.bytes.load(Ordering::Relaxed).to_le_bytes();
        let bytes = [a, b, c];
        let message = ShortMessage::new(bytes.get(..usize::from(len)).ok_or(MidiIoError::InvalidMessage("corrupt queued length"))?)?;
        let timestamp_us = slot.timestamp.load(Ordering::Relaxed);
        q.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(Some(TimedShortMessage { message, timestamp_us }))
    }
    fn overflow_count(&self) -> usize {
        self.0.overflow.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_short_packets() {
        for packet in [&[0x90, 60, 127][..], &[0xc0, 4], &[0xf2, 1, 2], &[0xf8], &[0xff]] {
            assert_eq!(ShortMessage::new(packet).unwrap().as_bytes(), packet);
        }
        for packet in [&[][..], &[60, 1], &[0x90, 1], &[0x90, 1, 128], &[0xf0], &[0xf4], &[0xf7], &[0xf9], &[0xfd]] {
            assert!(ShortMessage::new(packet).is_err());
        }
    }
    #[test]
    fn sysex_is_bounded_and_validated_before_copy() {
        assert_eq!(SysEx::new(&[0xf0, 1, 2, 0xf7]).unwrap().as_bytes(), [0xf0, 1, 2, 0xf7]);
        assert!(SysEx::new(&[0xf0, 0x80, 0xf7]).is_err());
        assert!(SysEx::new(&[0xf0]).is_err());
        assert!(SysEx::new(&vec![0; MAX_SYSEX_BYTES + 1]).is_err());
    }
    #[test]
    fn queue_preserves_order_and_rejects_overflow() {
        let (mut tx, mut rx) = input_queue(2).unwrap();
        let event = |n| TimedShortMessage { timestamp_us: n, message: ShortMessage::new(&[0x90, 60, 127]).unwrap() };
        assert_eq!(rx.try_receive().unwrap(), None);
        tx.try_send(event(1)).unwrap();
        tx.try_send(event(2)).unwrap();
        assert_eq!(tx.try_send(event(3)), Err(MidiIoError::QueueFull));
        assert_eq!(rx.overflow_count(), 1);
        assert_eq!(rx.try_receive().unwrap(), Some(event(1)));
        tx.try_send(event(4)).unwrap();
        assert_eq!(rx.try_receive().unwrap(), Some(event(2)));
        assert_eq!(rx.try_receive().unwrap(), Some(event(4)));
        assert_eq!(rx.try_receive().unwrap(), None);
        assert!(input_queue(0).is_err());
        assert!(input_queue(MAX_QUEUE_MESSAGES + 1).is_err());
    }
    #[test]
    fn queue_wraps_cursors_and_reports_disconnect_after_draining() {
        assert!(input_queue(3).is_err());
        let (mut tx, mut rx) = input_queue(4).unwrap();
        tx.0.head.store(usize::MAX - 1, Ordering::Relaxed);
        tx.0.tail.store(usize::MAX - 1, Ordering::Relaxed);
        for n in 0..4 {
            tx.try_send(TimedShortMessage { timestamp_us: n, message: ShortMessage::new(&[0xf8]).unwrap() }).unwrap();
        }
        drop(tx);
        for n in 0..4 {
            assert_eq!(rx.try_receive().unwrap().unwrap().timestamp_us, n);
        }
        assert_eq!(rx.try_receive(), Err(MidiIoError::Disconnected));
    }

    #[test]
    fn concurrent_queue_keeps_packets_and_timestamps_together() {
        let (mut tx, mut rx) = input_queue(16).unwrap();
        let producer = std::thread::spawn(move || {
            for n in 0..10_000u64 {
                let event = TimedShortMessage { timestamp_us: n, message: ShortMessage::new(&[0x90, (n % 128) as u8, 100]).unwrap() };
                while tx.try_send(event).is_err() {
                    std::thread::yield_now();
                }
            }
        });
        for n in 0..10_000u64 {
            let event = loop {
                match rx.try_receive().unwrap() {
                    Some(event) => break event,
                    None => std::thread::yield_now(),
                }
            };
            assert_eq!(event.timestamp_us, n);
            assert_eq!(event.message.as_bytes(), [0x90, (n % 128) as u8, 100]);
        }
        producer.join().unwrap();
    }

    #[test]
    fn unsupported_is_not_reported_as_hardware() {
        assert_eq!(UnsupportedMidiSystem.inputs(), Err(MidiIoError::Unsupported));
        assert_eq!(UnsupportedMidiSystem.outputs(), Err(MidiIoError::Unsupported));
        assert_eq!(MidiPortId::new(String::new()), Err(MidiIoError::InvalidPort));
    }
}

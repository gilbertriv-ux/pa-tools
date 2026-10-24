//! Live MIDI monitoring (G-02 MIDI thru): hardware MIDI played in and heard immediately on a
//! monitored Instrument track, without pressing record.
//!
//! The hand-off is a lock-free SPSC ring ([`rtrb`]):
//! - the **producer** ([`LiveMidiSender`]) is pushed from off the audio thread — the UI/input
//!   thread, which drains the hardware input's [`TimedMessage`](crate::midi_io::TimedMessage) queue
//!   each frame and forwards the bytes here;
//! - the **consumer** ([`LiveMidiReceiver`]) is drained on the audio thread at the start of each
//!   block into [`MixEngine::live_midi`](soundcraft_mix::MixEngine::live_midi), which the mix engine
//!   applies to monitored instruments (one block of latency).
//!
//! Neither side ever blocks. A full ring drops the message and counts it ([`LiveMidiSender::dropped`]);
//! draining never allocates, as it reuses the engine's pre-sized buffer.

use soundcraft_mix::LiveMidiMsg;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest live-MIDI ring accepted (messages). A keyboard played hard sends a few hundred messages
/// a second; a block is a few milliseconds, so this is a generous margin.
pub const MAX_CAPACITY: usize = 16_384;

/// Producer side, held by the UI/input thread. Cloneable count of drops is shared with nobody else;
/// it is read back for the status line.
pub struct LiveMidiSender {
    tx: rtrb::Producer<LiveMidiMsg>,
    dropped: Arc<AtomicU64>,
}

/// Consumer side, moved to the audio thread.
pub struct LiveMidiReceiver {
    rx: rtrb::Consumer<LiveMidiMsg>,
}

/// Create a live-MIDI ring holding `capacity` messages (clamped to `1..=`[`MAX_CAPACITY`]).
pub fn channel(capacity: usize) -> (LiveMidiSender, LiveMidiReceiver) {
    let cap = capacity.clamp(1, MAX_CAPACITY);
    let (tx, rx) = rtrb::RingBuffer::new(cap);
    (LiveMidiSender { tx, dropped: Arc::new(AtomicU64::new(0)) }, LiveMidiReceiver { rx })
}

impl LiveMidiSender {
    /// Push one raw hardware message. Non-blocking: a message that is not a short channel/realtime
    /// message, or that does not fit the ring, is dropped and counted. Safe to call from any
    /// non-audio thread.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        match LiveMidiMsg::new(bytes) {
            Some(m) if self.tx.push(m).is_ok() => {}
            _ => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Messages dropped because the ring was full or the bytes were not a usable short message.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl LiveMidiReceiver {
    /// Drain waiting messages into `out`, oldest first, up to its capacity. Clears `out` first and
    /// never grows it, so the caller's pre-sized buffer makes this allocation-free on the audio
    /// thread. Messages beyond the buffer's capacity stay in the ring for the next block.
    pub fn drain_into(&mut self, out: &mut Vec<LiveMidiMsg>) {
        out.clear();
        let cap = out.capacity();
        while out.len() < cap {
            match self.rx.pop() {
                Ok(m) => out.push(m),
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pushed_message_is_drained_in_order() {
        let (mut tx, mut rx) = channel(8);
        tx.push_bytes(&[0x90, 60, 100]);
        tx.push_bytes(&[0x80, 60, 0]);
        let mut buf = Vec::with_capacity(8);
        rx.drain_into(&mut buf);
        assert_eq!(buf.iter().map(|m| m.bytes().to_vec()).collect::<Vec<_>>(), vec![vec![0x90, 60, 100], vec![0x80, 60, 0]]);
        // Drained dry: the next call yields nothing.
        rx.drain_into(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn unusable_bytes_are_dropped_and_counted_not_queued() {
        let (mut tx, mut rx) = channel(8);
        tx.push_bytes(&[]); // empty
        tx.push_bytes(&[60, 100]); // no status byte
        tx.push_bytes(&[0x90, 60, 100, 1]); // too long for a channel message
        assert_eq!(tx.dropped(), 3);
        let mut buf = Vec::with_capacity(8);
        rx.drain_into(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_full_ring_drops_and_counts_without_blocking() {
        let (mut tx, mut rx) = channel(2);
        for _ in 0..5 {
            tx.push_bytes(&[0x90, 60, 100]);
        }
        assert_eq!(tx.dropped(), 3, "two fit, three overflow");
        let mut buf = Vec::with_capacity(8);
        rx.drain_into(&mut buf);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn drain_never_grows_the_buffer_and_leaves_the_rest_for_next_block() {
        let (mut tx, mut rx) = channel(16);
        for i in 0..6 {
            tx.push_bytes(&[0x90, 60 + i, 100]);
        }
        // A buffer that can hold only four leaves two in the ring for the next drain.
        let mut buf = Vec::with_capacity(4);
        let before = buf.capacity();
        rx.drain_into(&mut buf);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.capacity(), before, "no reallocation on the audio thread");
        rx.drain_into(&mut buf);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn capacity_is_clamped() {
        let (_tx, _rx) = channel(0); // does not panic (clamped to 1)
        let (_tx, _rx) = channel(usize::MAX); // clamped to MAX_CAPACITY
    }
}

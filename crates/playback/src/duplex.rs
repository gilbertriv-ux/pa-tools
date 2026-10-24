//! Safe ownership of paired input/output streams.
//!
//! The portable backend uses independent device clocks. Matching nominal rates does not
//! establish sample alignment; monitoring resamples through `InputRing`. Native ASIO shared
//! driver setup and hardware loopback verification remain separate work.
use crate::{AudioConfig, Player, latency::StreamLatency, record::Recorder};
use soundcraft_model::Session;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockRelationship {
    /// No shared hardware clock or sample alignment has been verified.
    Independent,
}

pub trait DuplexStream {
    fn player(&self) -> &Player;
    fn recorder(&self) -> &Recorder;
    fn recorder_mut(&mut self) -> &mut Recorder;
    fn clock_relationship(&self) -> ClockRelationship;
    fn input_latency(&self) -> Option<StreamLatency>;
    fn output_latency(&self) -> Option<StreamLatency>;
}

/// Owns both streams so failed input setup cannot leave a successful duplex result.
/// Recording continues to use a bounded ring and a disk writer.
pub struct PortableDuplex {
    recorder: Recorder,
    player: Player,
}

impl PortableDuplex {
    pub fn open(session: Arc<Session>, config: &AudioConfig) -> Result<Self, String> {
        if config.system.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("ASIO")) {
            return Err("native shared-clock duplex is not implemented for this audio system".into());
        }
        let player = Player::with_config(session, config);
        if player.silent {
            return Err("cannot open duplex without an audio output device".into());
        }
        let mut input = config.clone();
        input.sample_rate = Some(player.device_rate);
        input.buffer_size = player.device_buffer;
        let recorder = Recorder::open_with(&input)?;
        player.set_input(Arc::clone(&recorder.monitor));
        Ok(Self { recorder, player })
    }
}

impl DuplexStream for PortableDuplex {
    fn player(&self) -> &Player {
        &self.player
    }
    fn recorder(&self) -> &Recorder {
        &self.recorder
    }
    fn recorder_mut(&mut self) -> &mut Recorder {
        &mut self.recorder
    }
    fn clock_relationship(&self) -> ClockRelationship {
        ClockRelationship::Independent
    }
    fn input_latency(&self) -> Option<StreamLatency> {
        self.recorder.input_latency()
    }
    fn output_latency(&self) -> Option<StreamLatency> {
        self.player.output_latency()
    }
}

//! Safe native hardware MIDI adapter. Setup and output sends run on a MIDI worker.
//! Input callbacks copy only validated short messages into the preallocated atomic queue.
//! SysEx is deliberately ignored here; no callback-side packet allocation is performed.

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use crate::midi_devices::UnsupportedMidiSystem as NativeMidiSystem;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::NativeMidiSystem;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use crate::midi_devices::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SYSTEM: AtomicU64 = AtomicU64::new(1);
    const CLIENT: &str = "Pa Tools MIDI";

    /// Enumeration IDs expire on refresh. Stored backend handles are never replaced by
    /// another device merely because its name or enumeration position happens to match.
    pub struct NativeMidiSystem {
        instance: u64,
        input_epoch: u64,
        output_epoch: u64,
        inputs: Vec<(MidiPortId, midir::MidiInputPort)>,
        outputs: Vec<(MidiPortId, midir::MidiOutputPort)>,
    }
    impl Default for NativeMidiSystem {
        fn default() -> Self {
            Self::new()
        }
    }
    impl NativeMidiSystem {
        pub fn new() -> Self {
            Self {
                instance: NEXT_SYSTEM.fetch_add(1, Ordering::Relaxed),
                input_epoch: 0,
                output_epoch: 0,
                inputs: Vec::new(),
                outputs: Vec::new(),
            }
        }
    }
    fn backend(error: impl std::fmt::Display) -> MidiIoError {
        MidiIoError::Backend(error.to_string())
    }
    struct Input {
        // Keep the OS callback alive for exactly the receiver's lifetime.
        _connection: midir::MidiInputConnection<()>,
        receiver: Option<MidiInputReceiver>,
    }
    impl MidiInputConnection for Input {
        fn try_receive(&mut self) -> Result<Option<TimedShortMessage>, MidiIoError> {
            match self.receiver.as_mut() {
                Some(receiver) => receiver.try_receive(),
                None => Ok(None),
            }
        }
        fn overflow_count(&self) -> usize {
            self.receiver
                .as_ref()
                .map_or(0, MidiInputConnection::overflow_count)
        }
        fn take_receiver(&mut self) -> Option<MidiInputReceiver> {
            self.receiver.take()
        }
    }
    struct Output(midir::MidiOutputConnection);
    impl MidiOutputConnection for Output {
        fn send(&mut self, message: ShortMessage) -> Result<(), MidiIoError> {
            self.0.send(message.as_bytes()).map_err(backend)
        }
        fn send_sysex(&mut self, message: &SysEx) -> Result<(), MidiIoError> {
            self.0.send(message.as_bytes()).map_err(backend)
        }
    }
    impl MidiSystem for NativeMidiSystem {
        fn inputs(&mut self) -> Result<Vec<MidiInputPort>, MidiIoError> {
            let input = midir::MidiInput::new(CLIENT).map_err(backend)?;
            self.input_epoch = self
                .input_epoch
                .checked_add(1)
                .ok_or(MidiIoError::InvalidPort)?;
            let mut handles = Vec::new();
            let mut ports = Vec::new();
            for (index, port) in input.ports().into_iter().enumerate() {
                let name = input.port_name(&port).map_err(backend)?;
                let id = MidiPortId::new(format!(
                    "midir:{}:input:{}:{}",
                    self.instance, self.input_epoch, index
                ))?;
                handles.push((id.clone(), port));
                ports.push(MidiInputPort { id, name });
            }
            self.inputs = handles;
            Ok(ports)
        }
        fn outputs(&mut self) -> Result<Vec<MidiOutputPort>, MidiIoError> {
            let output = midir::MidiOutput::new(CLIENT).map_err(backend)?;
            self.output_epoch = self
                .output_epoch
                .checked_add(1)
                .ok_or(MidiIoError::InvalidPort)?;
            let mut handles = Vec::new();
            let mut ports = Vec::new();
            for (index, port) in output.ports().into_iter().enumerate() {
                let name = output.port_name(&port).map_err(backend)?;
                let id = MidiPortId::new(format!(
                    "midir:{}:output:{}:{}",
                    self.instance, self.output_epoch, index
                ))?;
                handles.push((id.clone(), port));
                ports.push(MidiOutputPort { id, name });
            }
            self.outputs = handles;
            Ok(ports)
        }
        fn open_input(
            &mut self,
            port: &MidiInputPort,
        ) -> Result<Box<dyn MidiInputConnection>, MidiIoError> {
            let handle = &self
                .inputs
                .iter()
                .find(|(id, _)| *id == port.id)
                .ok_or(MidiIoError::InvalidPort)?
                .1;
            let mut input = midir::MidiInput::new(CLIENT).map_err(backend)?;
            // Filter at the native backend as well as rejecting oversized callback packets.
            input.ignore(midir::Ignore::Sysex);
            input.port_name(handle).map_err(backend)?;
            let (mut sender, receiver) = input_queue(1024)?;
            let connection = input
                .connect(
                    handle,
                    CLIENT,
                    move |timestamp_us, bytes, _| {
                        forward_short(&mut sender, timestamp_us, bytes);
                    },
                    (),
                )
                .map_err(backend)?;
            Ok(Box::new(Input {
                _connection: connection,
                receiver: Some(receiver),
            }))
        }
        fn open_output(
            &mut self,
            port: &MidiOutputPort,
        ) -> Result<Box<dyn MidiOutputConnection>, MidiIoError> {
            let handle = &self
                .outputs
                .iter()
                .find(|(id, _)| *id == port.id)
                .ok_or(MidiIoError::InvalidPort)?
                .1;
            let output = midir::MidiOutput::new(CLIENT).map_err(backend)?;
            output.port_name(handle).map_err(backend)?;
            Ok(Box::new(Output(
                output.connect(handle, CLIENT).map_err(backend)?,
            )))
        }
    }
    fn forward_short(sender: &mut MidiInputSender, timestamp_us: u64, bytes: &[u8]) {
        if bytes.len() > 3 {
            return;
        }
        if let Ok(message) = ShortMessage::new(bytes) {
            let _ = sender.try_send(TimedShortMessage {
                timestamp_us,
                message,
            });
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn callback_discards_sysex_and_malformed_preserving_valid_order() {
            let (mut tx, mut rx) = input_queue(2).unwrap();
            forward_short(&mut tx, 1, &[0xf0, 1, 2, 0xf7]);
            forward_short(&mut tx, 2, &[0x90, 60]);
            forward_short(&mut tx, 3, &[0x90, 60, 100]);
            forward_short(&mut tx, 4, &[0x80, 60, 0]);
            forward_short(&mut tx, 5, &[0xf8]);
            assert_eq!(rx.overflow_count(), 1);
            assert_eq!(rx.try_receive().unwrap().unwrap().timestamp_us, 3);
            assert_eq!(rx.try_receive().unwrap().unwrap().timestamp_us, 4);
            assert_eq!(rx.try_receive().unwrap(), None);
        }
    }
}

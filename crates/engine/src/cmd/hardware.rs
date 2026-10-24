//! Hardware discovery commands: the MIDI ports the system offers.
//!
//! Listing is read-only and never opens a port. An unavailable MIDI backend (no sequencer, no
//! permission, a platform without MIDI) is reported as empty lists plus an `error` note, never as
//! a failure, so an agent can ask the same question on any machine. Measured audio latency needs
//! the live audio streams, which the engine does not own; it is read by the UI command
//! `setup.audio_latency`.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_playback::midi_io::{self, MidiIoError, MidiPortInfo};

/// Most ports reported per direction (a hostile or buggy backend cannot grow the reply).
pub const MAX_PORTS: usize = 256;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "setup.midi_ports",
        "MIDI Ports...",
        ["Setup", "MIDI"],
        None,
        "{} — returns {available, inputs: [{index, name}], outputs: [{index, name}], error?}: the MIDI ports the system offers (nothing is opened)",
        always,
        |_e, _p| Ok(ports_json(midi_io::list_inputs(), midi_io::list_outputs()))
    )]
}

fn port_list(r: &std::result::Result<Vec<MidiPortInfo>, MidiIoError>) -> Vec<Value> {
    r.as_ref().map(|v| v.iter().take(MAX_PORTS).map(|p| json!({"index": p.index, "name": p.name})).collect()).unwrap_or_default()
}

/// The reply of `setup.midi_ports`, from the two listings.
pub fn ports_json(
    inputs: std::result::Result<Vec<MidiPortInfo>, MidiIoError>,
    outputs: std::result::Result<Vec<MidiPortInfo>, MidiIoError>,
) -> Value {
    let (i, o) = (port_list(&inputs), port_list(&outputs));
    let mut v = json!({"available": !i.is_empty() || !o.is_empty(), "inputs": i, "outputs": o});
    let errors: Vec<String> = [inputs.err(), outputs.err()].into_iter().flatten().map(|e| e.to_string()).collect();
    if !errors.is_empty() {
        v["error"] = json!(errors.join("; "));
    }
    v
}

/// Names of the system's MIDI input ports (empty when there are none or the backend is down).
pub fn input_port_names() -> Vec<String> {
    midi_io::list_inputs().map(|v| v.into_iter().take(MAX_PORTS).map(|p| p.name).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(index: usize, name: &str) -> MidiPortInfo {
        MidiPortInfo { index, name: name.into() }
    }

    #[test]
    fn midi_ports_runs_without_hardware() {
        let mut e = Engine::default();
        let r = e.execute("setup.midi_ports", &json!({})).unwrap();
        assert!(r["inputs"].is_array() && r["outputs"].is_array());
        assert_eq!(r["available"], json!(!r["inputs"].as_array().unwrap().is_empty() || !r["outputs"].as_array().unwrap().is_empty()));
        assert!(!e.is_dirty(), "a query changes nothing");
    }

    #[test]
    fn empty_hardware_is_not_an_error() {
        let r = ports_json(Ok(vec![]), Ok(vec![]));
        assert_eq!(r, json!({"available": false, "inputs": [], "outputs": []}));
    }

    #[test]
    fn ports_are_listed_with_their_index() {
        let r = ports_json(Ok(vec![port(0, "Keys"), port(1, "Pads")]), Ok(vec![port(0, "Synth")]));
        assert_eq!(r["available"], true);
        assert_eq!(r["inputs"], json!([{"index": 0, "name": "Keys"}, {"index": 1, "name": "Pads"}]));
        assert_eq!(r["outputs"], json!([{"index": 0, "name": "Synth"}]));
        assert!(r.get("error").is_none());
    }

    #[test]
    fn backend_errors_become_a_note() {
        let r = ports_json(Err(MidiIoError::Backend("no sequencer".into())), Ok(vec![port(0, "Synth")]));
        assert_eq!(r["inputs"], json!([]));
        assert_eq!(r["outputs"].as_array().map(Vec::len), Some(1));
        assert!(r["error"].as_str().unwrap().contains("no sequencer"));
    }

    #[test]
    fn port_lists_are_bounded() {
        let many: Vec<MidiPortInfo> = (0..MAX_PORTS + 50).map(|i| port(i, "P")).collect();
        let r = ports_json(Ok(many), Ok(vec![]));
        assert_eq!(r["inputs"].as_array().map(Vec::len), Some(MAX_PORTS));
    }
}

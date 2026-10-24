//! Hardware-facing UI commands: measured audio latency and record-latency compensation.
//!
//! Both need the live audio streams (the player and the recorder), which only the UI owns, so
//! they are UI commands. `setup.midi_ports` (no live state needed) is an engine command.

use crate::SoundApp;
use serde_json::{Value, json};
use soundcraft_playback::latency;

/// `setup.record_latency_compensation`: `{value?: bool}` (toggles when omitted). Kept with the
/// preferences (per machine); off by default, which leaves takes where they were placed.
pub const COMPENSATION: &str = "setup.record_latency_compensation";
/// `setup.audio_latency`: read-only, the latency measured from the streams' timestamps.
pub const LATENCY: &str = "setup.audio_latency";

pub fn run(app: &mut SoundApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        COMPENSATION => {
            app.ui.compensate_latency = p.get("value").and_then(Value::as_bool).unwrap_or(!app.ui.compensate_latency);
            Some(Ok(json!({"value": app.ui.compensate_latency})))
        }
        LATENCY => Some(Ok(audio_latency(app))),
        _ => None,
    }
}

/// The reply of `setup.audio_latency`. `measured` is false until a stream callback has reported a
/// timestamp (and always for the silent clock used when no device opens), in which case the
/// figures are 0 rather than a guess.
pub fn audio_latency(app: &SoundApp) -> Value {
    let (out_frames, out_rate, buffer) =
        app.player.as_ref().filter(|p| !p.silent).map_or((0, 0, 0), |p| (p.output_latency_frames(), p.device_rate, p.device_buffer.unwrap_or(0)));
    let (in_frames, in_rate) = app.recorder.as_ref().map_or((0, 0), |r| (r.input_latency_frames(), r.sample_rate));
    let session_rate = app.engine.session().sample_rate.hz();
    let offset = record_offset(in_frames, in_rate, out_frames, out_rate, session_rate);
    let ms = |frames: u32, rate: u32| if rate == 0 { 0.0 } else { f64::from(frames) * 1000.0 / f64::from(rate) };
    json!({
        "measured": in_frames > 0 || out_frames > 0,
        "output": {"frames": out_frames, "sample_rate": out_rate, "ms": ms(out_frames, out_rate), "device_buffer_frames": buffer},
        "input": {"frames": in_frames, "sample_rate": in_rate, "ms": ms(in_frames, in_rate)},
        "round_trip_ms": ms(out_frames, out_rate) + ms(in_frames, in_rate),
        "record_offset_session_frames": offset,
        "session_sample_rate": session_rate,
        "compensation_enabled": app.ui.compensate_latency,
    })
}

/// The round-trip latency in session frames: input and output figures are in their own device's
/// frames, so each is converted to the session rate (checked, rounded to nearest). 0 when a rate
/// is unknown and its figure non-zero.
pub fn record_offset(in_frames: u32, in_rate: u32, out_frames: u32, out_rate: u32, session_rate: u32) -> i64 {
    let conv = |frames: u32, rate: u32| -> i64 {
        if frames == 0 || rate == 0 {
            return 0;
        }
        let n = i128::from(frames) * i128::from(session_rate) + i128::from(rate) / 2;
        i64::try_from(n / i128::from(rate)).unwrap_or(0)
    };
    conv(in_frames, in_rate).saturating_add(conv(out_frames, out_rate)).clamp(0, i64::from(latency::MAX_LATENCY_FRAMES) * 8)
}

/// Where a take armed at `start` goes: `(new start, frames to trim from its head)`. Unchanged
/// when compensation is off.
pub fn compensate(enabled: bool, start: i64, offset: i64) -> (i64, i64) {
    if enabled { (latency::shifted_start(start, offset), latency::head_trim(start, offset)) } else { (start, 0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;

    fn app() -> SoundApp {
        SoundApp::new(soundcraft_engine::Engine::default(), None, Services::default())
    }

    #[test]
    fn compensation_defaults_off_and_toggles() {
        let mut a = app();
        assert!(!a.ui.compensate_latency);
        assert_eq!(a.run(COMPENSATION, json!({})).unwrap(), json!({"value": true}));
        assert_eq!(a.run(COMPENSATION, json!({"value": false})).unwrap(), json!({"value": false}));
        assert_eq!(a.run(COMPENSATION, json!({"value": "x"})).unwrap(), json!({"value": true}), "a non-bool toggles");
    }

    #[test]
    fn compensation_persists_with_the_preferences_and_old_prefs_load_off() {
        let ui = crate::UiState { compensate_latency: true, ..Default::default() };
        let back: crate::UiState = serde_json::from_value(serde_json::to_value(&ui).unwrap()).unwrap();
        assert!(back.compensate_latency);
        let old: crate::UiState = serde_json::from_value(json!({})).unwrap();
        assert!(!old.compensate_latency);
    }

    #[test]
    fn latency_without_hardware_reports_unmeasured_zeros() {
        let a = app();
        let v = audio_latency(&a);
        assert_eq!(v["measured"], false);
        assert_eq!(v["input"]["frames"], 0);
        assert_eq!(v["output"]["frames"], 0);
        assert_eq!(v["round_trip_ms"], 0.0);
        assert_eq!(v["record_offset_session_frames"], 0);
        assert_eq!(v["compensation_enabled"], false);
    }

    #[test]
    fn latency_command_is_dispatchable() {
        let mut a = app();
        let v = a.run(LATENCY, json!({})).unwrap();
        assert!(v["measured"].is_boolean());
    }

    #[test]
    fn offsets_convert_between_rates_and_survive_hostile_input() {
        assert_eq!(record_offset(480, 48_000, 480, 48_000, 48_000), 960);
        assert_eq!(record_offset(480, 48_000, 0, 0, 96_000), 960, "input only, session twice the rate");
        assert_eq!(record_offset(100, 0, 100, 0, 48_000), 0, "unknown rates");
        assert!(record_offset(u32::MAX, 1, u32::MAX, 1, u32::MAX) >= 0);
        assert_eq!(record_offset(1, 8_000, 0, 0, 0), 0);
    }

    #[test]
    fn compensate_moves_earlier_and_trims_what_does_not_fit() {
        assert_eq!(compensate(false, 1_000, 200), (1_000, 0));
        assert_eq!(compensate(true, 1_000, 200), (800, 0));
        assert_eq!(compensate(true, 50, 200), (0, 150));
        assert_eq!(compensate(true, 0, 200), (0, 200));
        assert_eq!(compensate(true, 1_000, -5), (1_000, 0));
        assert_eq!(compensate(true, i64::MIN, i64::MAX).0, 0);
    }
}

//! Editing a stored transcript: naming its speakers (`track.rename_speaker`).
//!
//! The words and their times come from the speech model or from `track.set_transcript`; this module
//! only changes how the transcript is presented, so it needs no audio and stays undoable.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::Transcript;

const RENAME_SPEAKER: &str = "track.rename_speaker";

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "track.rename_speaker",
        "Rename Speaker",
        [],
        None,
        "{track: id|name, speaker: 0..255, name: text} — names a speaker in the track's transcript (at most 64 characters); \
         an empty name goes back to \"Speaker N\"",
        has_transcript,
        rename_speaker
    )]
}

fn has_transcript(e: &Engine) -> std::result::Result<(), String> {
    if e.session().transcripts.is_empty() { Err("no track has a transcript yet".into()) } else { Ok(()) }
}

fn rename_speaker(e: &mut Engine, p: &Value) -> Result<Value> {
    let track = track_param(e, RENAME_SPEAKER, p, "track")?.ok_or_else(|| bad(RENAME_SPEAKER, "`track` required"))?;
    let speaker = p
        .get("speaker")
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| bad(RENAME_SPEAKER, "`speaker` must be a whole number from 0 to 255"))?;
    let name = Transcript::clean_speaker_name(str_param(p, "name").ok_or_else(|| bad(RENAME_SPEAKER, "`name` must be text (empty resets it)"))?);

    let transcript = e.session().transcript(track).ok_or_else(|| bad(RENAME_SPEAKER, "that track has no transcript"))?;
    // Unchanged: leave the document alone, so no empty step lands in the undo history.
    if transcript.speaker_name(speaker) != name
        && let Some(t) = e.session_mut().transcripts.iter_mut().find(|t| t.track == track)
    {
        t.set_speaker_name(speaker, &name);
    }
    let label = e.session().transcript(track).map(|t| t.speaker_label(speaker)).unwrap_or_default();
    Ok(json!({"track": track.0, "speaker": speaker, "name": name, "label": label}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::demo::demo_engine;
    use soundcraft_model::TrackKind;

    fn first_audio(e: &Engine) -> TrackId {
        e.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).map(|t| t.id).unwrap()
    }

    fn with_transcript() -> (Engine, TrackId) {
        let mut e = demo_engine();
        let t = first_audio(&e);
        let words = json!([
            {"start": 0, "end": 12_000, "text": "hello", "speaker": 0},
            {"start": 48_000, "end": 60_000, "text": "there", "speaker": 1},
        ]);
        e.execute("track.set_transcript", &json!({"track": t.0, "words": words, "language": "en"})).unwrap();
        (e, t)
    }

    #[test]
    fn naming_a_speaker_is_stored_trimmed_and_undoable() {
        let (mut e, t) = with_transcript();
        let r = e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 1, "name": "  Dana  "})).unwrap();
        assert_eq!((r["name"].as_str(), r["label"].as_str()), (Some("Dana"), Some("Dana")));
        let got = e.session().transcript(t).unwrap();
        assert_eq!(got.speaker_label(1), "Dana");
        assert_eq!(got.speaker_label(0), "Speaker 0");
        assert_eq!(got.words.len(), 2, "the words are untouched");
        assert_eq!(e.undo_label(), Some("Rename Speaker"));

        assert!(e.undo());
        assert_eq!(e.session().transcript(t).unwrap().speaker_label(1), "Speaker 1");
        assert!(e.redo());
        assert_eq!(e.session().transcript(t).unwrap().speaker_label(1), "Dana");
    }

    #[test]
    fn an_empty_name_resets_and_the_track_can_be_given_by_name() {
        let (mut e, t) = with_transcript();
        let track_name = e.session().track(t).unwrap().name.clone();
        e.execute(RENAME_SPEAKER, &json!({"track": track_name, "speaker": 0, "name": "Ann"})).unwrap();
        assert_eq!(e.session().transcript(t).unwrap().speaker_label(0), "Ann");
        let r = e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 0, "name": "   "})).unwrap();
        assert_eq!(r["label"], "Speaker 0");
        assert!(e.session().transcript(t).unwrap().speaker_names.is_empty());
    }

    #[test]
    fn the_highest_speaker_id_works_and_one_past_it_is_refused() {
        let (mut e, t) = with_transcript();
        e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 255, "name": "Last"})).unwrap();
        assert_eq!(e.session().transcript(t).unwrap().speaker_label(255), "Last");
        let before = e.session().transcripts.clone();
        assert!(e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 256, "name": "x"})).is_err());
        assert_eq!(e.session().transcripts, before);
    }

    #[test]
    fn a_long_name_is_cut_and_control_characters_are_dropped() {
        let (mut e, t) = with_transcript();
        let long = format!("A\n\u{7}{}", "b".repeat(200));
        e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 0, "name": long})).unwrap();
        let name = e.session().transcript(t).unwrap().speaker_label(0);
        assert_eq!(name.chars().count(), Transcript::MAX_SPEAKER_NAME_CHARS);
        assert!(name.starts_with("Ab") && !name.chars().any(char::is_control));
    }

    #[test]
    fn bad_calls_are_refused_and_leave_the_session_alone() {
        let (mut e, t) = with_transcript();
        let other = e.session().tracks.iter().find(|x| x.kind == TrackKind::Audio && x.id != t).map(|x| x.id).unwrap();
        let before = e.session().transcripts.clone();
        let undo_len = e.undo_history().len();
        for bad_params in [
            json!({"speaker": 0, "name": "x"}),
            json!({"track": 999_999, "speaker": 0, "name": "x"}),
            json!({"track": "No Such Track", "speaker": 0, "name": "x"}),
            json!({"track": other.0, "speaker": 0, "name": "x"}),
            json!({"track": t.0, "speaker": 300, "name": "x"}),
            json!({"track": t.0, "speaker": -1, "name": "x"}),
            json!({"track": t.0, "speaker": 1.5, "name": "x"}),
            json!({"track": t.0, "speaker": "1", "name": "x"}),
            json!({"track": t.0, "name": "x"}),
            json!({"track": t.0, "speaker": 0}),
            json!({"track": t.0, "speaker": 0, "name": 7}),
        ] {
            assert!(e.execute(RENAME_SPEAKER, &bad_params).is_err(), "{bad_params}");
        }
        assert_eq!(e.session().transcripts, before);
        assert_eq!(e.undo_history().len(), undo_len, "refused calls add no undo step");
    }

    #[test]
    fn a_session_without_transcripts_disables_the_command() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        assert!(has_transcript(&e).is_err());
        let err = e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 0, "name": "x"})).unwrap_err();
        assert!(err.to_string().contains("no transcript"), "{err}");
    }

    #[test]
    fn naming_a_speaker_the_same_way_twice_adds_one_undo_step() {
        let (mut e, t) = with_transcript();
        let base = e.undo_history().len();
        for _ in 0..2 {
            e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 0, "name": "Ann"})).unwrap();
        }
        assert_eq!(e.undo_history().len(), base + 1);
        // Resetting a speaker that was never named is a no-op as well.
        e.execute(RENAME_SPEAKER, &json!({"track": t.0, "speaker": 5, "name": ""})).unwrap();
        assert_eq!(e.undo_history().len(), base + 1);
    }

    #[test]
    fn the_command_is_registered_and_listed_as_journaled_and_undoable() {
        let spec = find_command(RENAME_SPEAKER).unwrap();
        assert!(spec.journal && spec.undoable && !spec.view);
    }
}

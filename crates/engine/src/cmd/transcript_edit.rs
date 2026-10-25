//! Editing a stored transcript: naming its speakers (`track.rename_speaker`) and correcting a
//! word (`track.edit_word`).
//!
//! The words and their times come from the speech model or from `track.set_transcript`; this module
//! only changes how the transcript is presented, so it needs no audio and stays undoable.

use super::*;
use crate::cmd;
use serde_json::json;
use soundcraft_model::Transcript;

const RENAME_SPEAKER: &str = "track.rename_speaker";
const EDIT_WORD: &str = "track.edit_word";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "track.rename_speaker",
            "Rename Speaker",
            [],
            None,
            "{track: id|name, speaker: 0..255, name: text} — names a speaker in the track's transcript (at most 64 characters); \
             an empty name goes back to \"Speaker N\"",
            has_transcript,
            rename_speaker
        ),
        cmd!(
            "track.edit_word",
            "Edit Word",
            [],
            None,
            "{track: id|name, index: 0.., text: text} — corrects the text of one word in the track's transcript (trimmed, at \
             most 128 characters); the word's time and speaker are unchanged. An empty or unchanged text does nothing",
            has_transcript,
            edit_word
        ),
    ]
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

fn edit_word(e: &mut Engine, p: &Value) -> Result<Value> {
    let track = track_param(e, EDIT_WORD, p, "track")?.ok_or_else(|| bad(EDIT_WORD, "`track` required"))?;
    let index = p
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| bad(EDIT_WORD, "`index` must be a whole number (the word's position)"))?;
    let text = str_param(p, "text").ok_or_else(|| bad(EDIT_WORD, "`text` must be text"))?;

    let transcript = e.session().transcript(track).ok_or_else(|| bad(EDIT_WORD, "that track has no transcript"))?;
    let normalized: String = text.trim().chars().take(128).collect();
    let current = transcript.words.get(index).map(|w| w.text.clone());
    let Some(current) = current else {
        return Err(bad(EDIT_WORD, "`index` is past the end of the transcript"));
    };
    // Unchanged or empty: leave the document alone, so no empty step lands in the undo history.
    if !normalized.is_empty()
        && current != normalized
        && let Some(t) = e.session_mut().transcripts.iter_mut().find(|t| t.track == track)
    {
        t.set_word_text(index, &normalized);
    }
    let word = e.session().transcript(track).and_then(|t| t.words.get(index)).map(|w| w.text.clone()).unwrap_or_default();
    Ok(json!({"track": track.0, "index": index, "text": word}))
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

    #[test]
    fn editing_a_word_is_stored_trimmed_and_undoable() {
        let (mut e, t) = with_transcript();
        let r = e.execute(EDIT_WORD, &json!({"track": t.0, "index": 0, "text": "  Hello  "})).unwrap();
        assert_eq!((r["index"].as_u64(), r["text"].as_str()), (Some(0), Some("Hello")));
        let got = e.session().transcript(t).unwrap();
        assert_eq!(got.words[0].text, "Hello");
        assert_eq!(got.words[1].text, "there", "the other words are untouched");
        assert_eq!(got.words[0].start, 0, "the word's time and speaker are untouched");
        assert_eq!(got.words[1].speaker, 1);
        assert_eq!(e.undo_label(), Some("Edit Word"));

        assert!(e.undo());
        assert_eq!(e.session().transcript(t).unwrap().words[0].text, "hello");
        assert!(e.redo());
        assert_eq!(e.session().transcript(t).unwrap().words[0].text, "Hello");
    }

    #[test]
    fn a_word_can_be_edited_with_the_track_given_by_name() {
        let (mut e, t) = with_transcript();
        let track_name = e.session().track(t).unwrap().name.clone();
        e.execute(EDIT_WORD, &json!({"track": track_name, "index": 1, "text": "world"})).unwrap();
        assert_eq!(e.session().transcript(t).unwrap().words[1].text, "world");
    }

    #[test]
    fn an_index_past_the_end_is_refused_and_leaves_the_session_alone() {
        let (mut e, t) = with_transcript();
        let before = e.session().transcripts.clone();
        let undo_len = e.undo_history().len();
        assert!(e.execute(EDIT_WORD, &json!({"track": t.0, "index": 2, "text": "x"})).is_err());
        assert!(e.execute(EDIT_WORD, &json!({"track": t.0, "index": 999, "text": "x"})).is_err());
        assert_eq!(e.session().transcripts, before);
        assert_eq!(e.undo_history().len(), undo_len, "a refused call adds no undo step");
    }

    #[test]
    fn an_empty_or_unchanged_edit_does_nothing() {
        let (mut e, t) = with_transcript();
        let base = e.undo_history().len();
        // Unchanged (after trimming): no change, no undo step, but the call still succeeds.
        let r = e.execute(EDIT_WORD, &json!({"track": t.0, "index": 0, "text": "  hello "})).unwrap();
        assert_eq!(r["text"], "hello");
        assert_eq!(e.undo_history().len(), base);
        // Empty or whitespace-only: refused to blank a word, so it stays as it was.
        for blank in ["", "   ", "\t\n"] {
            e.execute(EDIT_WORD, &json!({"track": t.0, "index": 0, "text": blank})).unwrap();
        }
        assert_eq!(e.session().transcript(t).unwrap().words[0].text, "hello");
        assert_eq!(e.undo_history().len(), base);
    }

    #[test]
    fn a_long_correction_is_cut_at_a_character_boundary() {
        let (mut e, t) = with_transcript();
        let long = "é".repeat(200);
        e.execute(EDIT_WORD, &json!({"track": t.0, "index": 0, "text": long})).unwrap();
        assert_eq!(e.session().transcript(t).unwrap().words[0].text.chars().count(), 128);
    }

    #[test]
    fn bad_edit_word_calls_are_refused_and_leave_the_session_alone() {
        let (mut e, t) = with_transcript();
        let other = e.session().tracks.iter().find(|x| x.kind == TrackKind::Audio && x.id != t).map(|x| x.id).unwrap();
        let before = e.session().transcripts.clone();
        let undo_len = e.undo_history().len();
        for bad_params in [
            json!({"index": 0, "text": "x"}),
            json!({"track": 999_999, "index": 0, "text": "x"}),
            json!({"track": "No Such Track", "index": 0, "text": "x"}),
            json!({"track": other.0, "index": 0, "text": "x"}),
            json!({"track": t.0, "index": -1, "text": "x"}),
            json!({"track": t.0, "index": 1.5, "text": "x"}),
            json!({"track": t.0, "index": "0", "text": "x"}),
            json!({"track": t.0, "text": "x"}),
            json!({"track": t.0, "index": 0}),
            json!({"track": t.0, "index": 0, "text": 7}),
        ] {
            assert!(e.execute(EDIT_WORD, &bad_params).is_err(), "{bad_params}");
        }
        assert_eq!(e.session().transcripts, before);
        assert_eq!(e.undo_history().len(), undo_len, "refused calls add no undo step");
    }

    #[test]
    fn a_session_without_transcripts_disables_edit_word() {
        let mut e = demo_engine();
        let t = first_audio(&e);
        assert!(has_transcript(&e).is_err());
        let err = e.execute(EDIT_WORD, &json!({"track": t.0, "index": 0, "text": "x"})).unwrap_err();
        assert!(err.to_string().contains("no transcript"), "{err}");
    }

    #[test]
    fn the_edit_word_command_is_registered_and_listed_as_journaled_and_undoable() {
        let spec = find_command(EDIT_WORD).unwrap();
        assert!(spec.journal && spec.undoable && !spec.view);
    }
}

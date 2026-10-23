//! Decoding Whisper token ids to text from the model's `tokenizer.json` (byte-level BPE: each
//! vocabulary entry is a string of "printable" stand-in characters, one per byte, using the
//! GPT-2 byte↔unicode table; special tokens are listed separately as added tokens).
//!
//! Only decoding is needed (Whisper produces tokens; we never encode text), so the BPE merges are
//! not used.

use std::collections::HashMap;

use crate::SpeechError;

pub struct Tokenizer {
    /// Bytes of each regular token, by id.
    bytes: Vec<Vec<u8>>,
    /// Special (added) tokens by content and by id.
    special: HashMap<String, u32>,
    special_by_id: HashMap<u32, String>,
    pub eot: u32,
    pub sot: u32,
    pub transcribe: u32,
    pub translate: Option<u32>,
    pub no_timestamps: u32,
    pub no_speech: Option<u32>,
    /// `<|0.00|>`; every later id is a timestamp in 0.02 s steps.
    pub timestamp_begin: u32,
    /// Language tokens (`"en"` → id), multilingual models only.
    pub languages: Vec<(String, u32)>,
}

/// GPT-2's byte → printable character table, inverted.
fn unicode_to_byte() -> HashMap<char, u8> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    // every `c` here is a valid scalar value by construction; drop any that somehow isn't rather
    // than panic (the crate forbids `expect`/`unwrap`).
    bs.iter().zip(&cs).filter_map(|(&b, &c)| char::from_u32(c).map(|ch| (ch, b as u8))).collect()
}

/// Most entries a tokenizer file may declare (real vocabularies hold about 52 000). A number read
/// from the file never sizes an allocation past this.
pub const MAX_VOCAB: usize = 262_144;

/// Timestamp tokens in a model's vocabulary: `<|0.00|>` to `<|30.00|>` in 0.02 s steps.
pub const TIMESTAMP_COUNT: usize = 1501;

/// The longest language code accepted for a language token (`haw`, `yue`, ...).
const MAX_LANGUAGE_CODE: usize = 8;

/// The language code inside a `<|xx|>` token, if it has one. `<|>` and `<|en` have none.
fn language_code(token: &str) -> Option<&str> {
    token.strip_prefix("<|")?.strip_suffix("|>")
}

/// The step (0..=1500, in 0.02 s) of a timestamp token `<|S.SS|>`, if `token` is one. A token
/// that is shaped like a timestamp but is not on the 0.02 s grid (`<|0.01|>`, `<|31.00|>`) is
/// `Some(Err(()))`: it is neither a timestamp nor an ordinary special token.
fn timestamp_step(token: &str) -> Option<Result<usize, ()>> {
    let (secs, hundredths) = language_code(token)?.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(secs) || hundredths.len() != 2 || !digits(hundredths) {
        return None;
    }
    let (secs, hundredths): (usize, usize) = (secs.parse().ok()?, hundredths.parse().ok()?);
    Some(if hundredths % 2 == 0 && secs * 50 + hundredths / 2 < TIMESTAMP_COUNT { Ok(secs * 50 + hundredths / 2) } else { Err(()) })
}

fn plausible_language_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= MAX_LANGUAGE_CODE && code.bytes().all(|b| b.is_ascii_lowercase())
}

impl Tokenizer {
    /// Parse `tokenizer.json`. `vocab_size` is the model's (from `config.json`): every token id in
    /// the file must lie below it, and the file may not declare more tokens than that, so a damaged
    /// or hostile file is refused instead of sizing tables from its numbers.
    pub fn from_json(json: &str, vocab_size: usize) -> Result<Self, SpeechError> {
        let bad = |m: &str| SpeechError::Model(format!("tokenizer.json: {m}"));
        if vocab_size == 0 || vocab_size > MAX_VOCAB {
            return Err(bad(&format!("vocab_size {vocab_size} is outside 1..={MAX_VOCAB}")));
        }
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?;
        let vocab = v["model"]["vocab"].as_object().ok_or_else(|| bad("no model.vocab"))?;
        if vocab.len() > vocab_size {
            return Err(bad(&format!("{} vocabulary entries, but the model's vocab_size is {vocab_size}", vocab.len())));
        }
        let table = unicode_to_byte();
        let mut bytes = vec![Vec::new(); vocab_size];
        let mut highest_text_id = 0usize;
        for (tok, id) in vocab {
            let id = id.as_u64().ok_or_else(|| bad("vocab id is not a whole number"))?;
            let index = usize::try_from(id).ok().filter(|i| *i < vocab_size);
            let (Some(index), Some(slot)) = (index, index.and_then(|i| bytes.get_mut(i))) else {
                return Err(bad(&format!("vocab id {id} is outside 0..{vocab_size}")));
            };
            highest_text_id = highest_text_id.max(index);
            *slot = tok.chars().filter_map(|c| table.get(&c).copied()).collect();
        }
        let added = v["added_tokens"].as_array().ok_or_else(|| bad("no added_tokens"))?;
        if added.len() > vocab_size {
            return Err(bad(&format!("{} added tokens, but the model's vocab_size is {vocab_size}", added.len())));
        }
        let mut special = HashMap::new();
        let mut special_by_id = HashMap::new();
        for a in added {
            let (Some(id), Some(c)) = (a["id"].as_u64(), a["content"].as_str()) else { continue };
            let id = u32::try_from(id).ok().filter(|&i| (i as usize) < vocab_size);
            let Some(id) = id else { return Err(bad(&format!("added token {c:?} has an id outside 0..{vocab_size}"))) };
            special.insert(c.to_string(), id);
            special_by_id.insert(id, c.to_string());
        }
        let get = |k: &str| special.get(k).copied();
        let need = |k: &str| get(k).ok_or_else(|| bad(&format!("no {k}")));
        let sot = need("<|startoftranscript|>")?;
        let translate = get("<|translate|>");
        let mut languages: Vec<(String, u32)> = Vec::new();
        // Between the start token and the translate token only language tokens live (none at all
        // in an English-only model, whose start and translate tokens are adjacent).
        for (k, &id) in special.iter().filter(|(_, id)| translate.is_some_and(|t| **id > sot && **id < t)) {
            match language_code(k).filter(|c| plausible_language_code(c)) {
                Some(code) => languages.push((code.to_string(), id)),
                None => return Err(bad(&format!("language token {k:?} is malformed"))),
            }
        }
        languages.sort_by_key(|x| x.1);
        let timestamp_begin = need("<|0.00|>")?;
        check_timestamp_layout(&special, timestamp_begin, highest_text_id, vocab_size).map_err(|why| bad(&why))?;
        Ok(Self {
            eot: need("<|endoftext|>")?,
            sot,
            transcribe: need("<|transcribe|>")?,
            translate,
            no_timestamps: need("<|notimestamps|>")?,
            no_speech: get("<|nospeech|>").or_else(|| get("<|nocaptions|>")),
            timestamp_begin,
            languages,
            bytes,
            special,
            special_by_id,
        })
    }

    pub fn multilingual(&self) -> bool {
        !self.languages.is_empty()
    }

    pub fn language_token(&self, code: &str) -> Option<u32> {
        self.languages.iter().find(|(c, _)| c == code).map(|x| x.1)
    }

    pub fn is_timestamp(&self, id: u32) -> bool {
        id >= self.timestamp_begin
    }

    /// Bytes of a regular token (empty for special tokens).
    pub fn token_bytes(&self, id: u32) -> &[u8] {
        if self.special_by_id.contains_key(&id) {
            return &[];
        }
        self.bytes.get(id as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        let b: Vec<u8> = ids.iter().flat_map(|&i| self.token_bytes(i).iter().copied()).collect();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Ids of all special tokens that must never be sampled as text (everything special except
    /// end-of-text and the timestamps).
    pub fn non_text_specials(&self) -> Vec<u32> {
        self.special.values().copied().filter(|&id| id != self.eot && id < self.timestamp_begin).collect()
    }
}

/// The timestamp tokens must be what the decoder assumes they are: one run of [`TIMESTAMP_COUNT`]
/// consecutive ids starting at `<|0.00|>`, inside the vocabulary and above every other token, so
/// that "id >= `timestamp_begin`" means "timestamp" and nothing else does. A file that breaks this
/// (say `<|0.00|>` moved to id 0, making every token a timestamp) would make the decoder step the
/// audio forward 10 ms at a time, for hours; it is refused when the model is loaded.
fn check_timestamp_layout(special: &HashMap<String, u32>, timestamp_begin: u32, highest_text_id: usize, vocab_size: usize) -> Result<(), String> {
    let begin = timestamp_begin as usize;
    if begin.saturating_add(TIMESTAMP_COUNT) > vocab_size {
        return Err(format!("the {TIMESTAMP_COUNT} timestamp tokens starting at <|0.00|> (id {begin}) do not fit in a vocabulary of {vocab_size}"));
    }
    if highest_text_id >= begin {
        return Err(format!("text token {highest_text_id} lies at or after <|0.00|> (id {begin}): timestamps must come after every other token"));
    }
    for (content, &id) in special {
        match timestamp_step(content) {
            Some(Ok(step)) if id as usize == begin + step => {}
            Some(Ok(step)) => {
                return Err(format!(
                    "{content} has id {id}, but the timestamp tokens run consecutively from <|0.00|> (id {begin}): expected {}",
                    begin + step
                ));
            }
            Some(Err(())) => return Err(format!("{content} is not a timestamp on the 0.02 s grid between 0.00 and 30.00")),
            None if id as usize >= begin => {
                return Err(format!(
                    "{content} (id {id}) lies at or after <|0.00|> (id {begin}): timestamps must come after every other special token"
                ));
            }
            None => {}
        }
    }
    Ok(())
}

/// Group text tokens into words: a token whose text starts with a space starts a new word;
/// punctuation-only tokens join the word before them (opening quotes/brackets the word after).
/// Returns (word text, token index range).
pub fn group_words(tok: &Tokenizer, ids: &[u32]) -> Vec<(String, std::ops::Range<usize>)> {
    let mut groups: Vec<(Vec<u8>, std::ops::Range<usize>)> = Vec::new();
    let mut pending_open: Option<(Vec<u8>, usize)> = None;
    for (i, &id) in ids.iter().enumerate() {
        let b = tok.token_bytes(id);
        if b.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(b);
        let trimmed = text.trim();
        let starts_space = b.first() == Some(&b' ');
        let punct = !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_punctuation() || "“”‘’«»¿¡…。，！？：、".contains(c));
        let opening = punct && trimmed.chars().all(|c| "\"'“‘¿¡([{«-".contains(c)) && starts_space;
        if opening {
            let start = pending_open.as_ref().map(|p| p.1).unwrap_or(i);
            let mut acc = pending_open.take().map(|p| p.0).unwrap_or_default();
            acc.extend_from_slice(b);
            pending_open = Some((acc, start));
            continue;
        }
        if let Some((mut pre, start)) = pending_open.take() {
            pre.extend_from_slice(b);
            groups.push((pre, start..i + 1));
            continue;
        }
        match groups.last_mut() {
            Some(g) if !starts_space => {
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            Some(g) if punct => {
                // " ," style punctuation with a space still belongs to the previous word
                g.0.extend_from_slice(b);
                g.1.end = i + 1;
            }
            _ => groups.push((b.to_vec(), i..i + 1)),
        }
    }
    if let Some((pre, start)) = pending_open {
        groups.push((pre, start..ids.len()));
    }
    groups.into_iter().map(|(b, r)| (String::from_utf8_lossy(&b).trim().to_string(), r)).filter(|(t, _)| !t.is_empty()).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// The vocabulary size of the small tokenizers below: text below 90, specials 90..=99, then
    /// the timestamp run from 100 (so it needs `100 + 1501` ids).
    const VOCAB: usize = 1700;

    /// A small multilingual tokenizer: ids below 90 are text, specials at 90..=99, `<|0.00|>` at 100.
    fn json(vocab: &str, extra: &str) -> String {
        format!(
            r#"{{"model": {{"vocab": {vocab}}}, "added_tokens": [
                {{"id": 90, "content": "<|endoftext|>"}}, {{"id": 91, "content": "<|startoftranscript|>"}},
                {{"id": 92, "content": "<|en|>"}}, {{"id": 93, "content": "<|translate|>"}},
                {{"id": 94, "content": "<|transcribe|>"}}, {{"id": 95, "content": "<|notimestamps|>"}},
                {{"id": 100, "content": "<|0.00|>"}}, {{"id": 101, "content": "<|0.02|>"}}{extra}]}}"#
        )
    }

    #[test]
    fn a_well_formed_tokenizer_parses() {
        let t = Tokenizer::from_json(&json(r#"{"a": 0, "b": 1}"#, ""), VOCAB).unwrap();
        assert_eq!((t.eot, t.sot, t.timestamp_begin), (90, 91, 100));
        assert_eq!(t.languages, vec![("en".to_string(), 92)]);
        assert_eq!(t.decode(&[0, 1, 90]), "ab");
        // The run ends exactly at the vocabulary's end.
        assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, ""), 100 + TIMESTAMP_COUNT).is_ok());
    }

    #[test]
    fn ids_at_or_past_vocab_size_are_refused() {
        for id in [VOCAB as u64, 1 << 32, 1_000_000_000_000_000] {
            let err = Tokenizer::from_json(&json(&format!(r#"{{"a": {id}}}"#), ""), VOCAB).err().unwrap();
            assert!(err.to_string().contains("vocab id"), "{err}");
            let extra = format!(r#", {{"id": {id}, "content": "<|zz|>"}}"#);
            let err = Tokenizer::from_json(&json(r#"{"a": 0}"#, &extra), VOCAB).err().unwrap();
            assert!(err.to_string().contains("outside"), "{err}");
        }
        assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, ""), 0).is_err());
        assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, ""), MAX_VOCAB + 1).is_err());
    }

    #[test]
    fn malformed_language_tokens_are_refused_not_sliced() {
        for content in ["<|>", "<|", "|>", "<|EN|>", "<|a b|>", "<||>", "<|waytoolongcode|>"] {
            let extra = format!(r#", {{"id": 97, "content": {}}}"#, serde_json::to_string(content).unwrap());
            // 97 is outside (sot, translate) = (91, 93): only tokens in that range are language tokens
            assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, &extra), VOCAB).is_ok(), "{content}");
            let in_range = json(r#"{"a": 0}"#, "")
                .replace(r#""id": 92, "content": "<|en|>""#, &format!(r#""id": 92, "content": {}"#, serde_json::to_string(content).unwrap()));
            let err = Tokenizer::from_json(&in_range, VOCAB).err().unwrap_or_else(|| panic!("{content} was accepted as a language"));
            assert!(err.to_string().contains("language token"), "{err}");
        }
    }

    #[test]
    fn more_entries_than_vocab_size_are_refused() {
        let err = Tokenizer::from_json(&json(r#"{"a": 0, "b": 1, "c": 2}"#, ""), 2).err().unwrap();
        assert!(err.to_string().contains("vocab"), "{err}");
    }

    /// Replace `<|0.00|>`'s id (the shape of the hostile file that made every token a timestamp).
    fn with_timestamp_begin(id: u32) -> String {
        json(r#"{"a": 0, "b": 1}"#, "").replace(r#""id": 100, "content": "<|0.00|>""#, &format!(r#""id": {id}, "content": "<|0.00|>""#))
    }

    #[test]
    fn timestamps_moved_below_the_other_tokens_are_refused() {
        // `<|0.00|>` at id 0: "id >= timestamp_begin" would be true of every token.
        for id in [0u32, 1, 50, 89, 93, 99] {
            let err = Tokenizer::from_json(&with_timestamp_begin(id), VOCAB).err().unwrap_or_else(|| panic!("<|0.00|> at {id} was accepted"));
            assert!(err.to_string().contains("timestamp"), "{id}: {err}");
        }
    }

    #[test]
    fn a_timestamp_run_that_does_not_fit_the_vocabulary_is_refused() {
        // 1700 ids hold the run, 1600 do not (100 + 1501 > 1600).
        let err = Tokenizer::from_json(&json(r#"{"a": 0}"#, ""), 1600).err().unwrap();
        assert!(err.to_string().contains("do not fit"), "{err}");
        assert!(Tokenizer::from_json(&with_timestamp_begin(VOCAB as u32 - 10), VOCAB).is_err());
    }

    #[test]
    fn a_timestamp_off_the_run_or_the_grid_is_refused() {
        for extra in [
            r#", {"id": 150, "content": "<|0.04|>"}"#,  // not at begin + 2
            r#", {"id": 103, "content": "<|0.05|>"}"#,  // not on the 0.02 s grid
            r#", {"id": 103, "content": "<|31.00|>"}"#, // past 30 s
            r#", {"id": 1700, "content": "<|0.06|>"}"#, // outside the vocabulary
        ] {
            assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, extra), VOCAB).is_err(), "{extra}");
        }
        // The right place is fine.
        assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, r#", {"id": 102, "content": "<|0.04|>"}"#), VOCAB).is_ok());
        assert!(Tokenizer::from_json(&json(r#"{"a": 0}"#, r#", {"id": 1600, "content": "<|30.00|>"}"#), VOCAB).is_ok());
    }

    #[test]
    fn nothing_else_may_sit_inside_or_after_the_timestamp_run() {
        // Another special token past `<|0.00|>`.
        let err = Tokenizer::from_json(&json(r#"{"a": 0}"#, r#", {"id": 500, "content": "<|startoflm|>"}"#), VOCAB).err().unwrap();
        assert!(err.to_string().contains("after every other special token"), "{err}");
        // A text token past `<|0.00|>`.
        let err = Tokenizer::from_json(&json(r#"{"a": 0, "late": 600}"#, ""), VOCAB).err().unwrap();
        assert!(err.to_string().contains("after every other token"), "{err}");
    }

    #[test]
    fn timestamp_tokens_are_recognised_by_shape() {
        assert_eq!(timestamp_step("<|0.00|>"), Some(Ok(0)));
        assert_eq!(timestamp_step("<|0.02|>"), Some(Ok(1)));
        assert_eq!(timestamp_step("<|1.00|>"), Some(Ok(50)));
        assert_eq!(timestamp_step("<|30.00|>"), Some(Ok(1500)));
        assert_eq!(timestamp_step("<|30.02|>"), Some(Err(())));
        assert_eq!(timestamp_step("<|0.01|>"), Some(Err(())));
        for not in ["<|en|>", "<|notimestamps|>", "<|0.0|>", "<|0.000|>", "<|.50|>", "<|1.2x|>", "0.00", "<|-1.00|>", "<|100.00|>"] {
            assert_eq!(timestamp_step(not), None, "{not}");
        }
    }
}

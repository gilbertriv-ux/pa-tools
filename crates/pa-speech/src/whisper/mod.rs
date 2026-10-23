//! Whisper speech recognition in pure Rust (candle, CPU).
//!
//! Transcription follows the procedure described with the model (sequential 30-second windows):
//!
//! 1. The whole clip's log-mel spectrogram is computed once ([`crate::mel`]), padded with 30 s of
//!    silence. A window of 3000 frames starting at `seek` is encoded.
//! 2. **Language**: unless given, the first window's decoder logits after `<|startoftranscript|>`
//!    are compared over the language tokens and the most likely language is used.
//! 3. **Decoding** is greedy with timestamp tokens: the first token must be a timestamp (≤ 1 s),
//!    timestamps come in pairs and never go backwards, timestamps win whenever their summed
//!    probability beats the best text token, and special tokens are suppressed. Windows whose
//!    `<|nospeech|>` probability exceeds 0.6 while the text is improbable are skipped. A loop
//!    guard stops a window that keeps repeating itself.
//! 4. `seek` advances to the last complete timestamp pair (or the whole window).
//! 5. **Word timestamps**: the window's text tokens are run once more through the decoder
//!    (`<|notimestamps|>` prompt) and the cross-attention logits of the model's alignment heads
//!    are softmaxed over the window's audio frames, standardised per token, median-filtered
//!    (width 7) and averaged; dynamic time warping through the negated matrix gives each token's
//!    start frame (20 ms resolution). Tokens are grouped into words at spaces.
//! 6. Word bounds are tightened past silent frames ([`crate::vad`]).
//! 7. Optional speaker labelling runs afterwards ([`crate::diarize`]).
//!
//! The public [`crate::Transcriber`] returns the crate's `f64`-seconds transcript; internally the
//! recogniser works in exact ticks ([`crate::ptranscript`]) and converts at the boundary.

mod align;
mod model;
pub mod tokenizer;

use std::path::Path;
use std::sync::Mutex;

use candle_core::Tensor;

use crate::ptranscript::{Transcript, Word};
use crate::tick::sample_tick;
use crate::{Options, ProgressFn, SpeechError};
use model::{Config, Model};
use tokenizer::Tokenizer;

/// Mel frames per window (30 s).
const N_FRAMES: usize = 3000;
/// Mel frames per timestamp step (0.02 s).
const FRAMES_PER_TS: usize = 2;
const SAMPLES_PER_FRAME: usize = crate::mel::HOP;
const MAX_TOKENS: usize = 224;

fn merr(e: candle_core::Error) -> SpeechError {
    SpeechError::Model(e.to_string())
}

/// The error for numbers that are not numbers: a network whose weights are all finite can still
/// overflow while it runs (a weight of 3e38 makes the first layer's sums infinite), and what it
/// then answers is noise that reads as a transcript (`!!!!!!!!!!`). `what` says where it was seen.
fn invalid_numbers(what: &str) -> SpeechError {
    SpeechError::Model(format!(
        "the speech model produced invalid numbers in its {what} (not a number, or too large); its files may be damaged, so nothing was transcribed"
    ))
}

/// `Ok` when every value is finite, else [`invalid_numbers`] for `what`.
fn ensure_finite(values: &[f32], what: &str) -> Result<(), SpeechError> {
    if values.iter().all(|v| v.is_finite()) { Ok(()) } else { Err(invalid_numbers(what)) }
}

/// [`ensure_finite`] for a whole tensor (the encoder's output: 1500 x width values per window).
fn ensure_tensor_finite(t: &Tensor, what: &str) -> Result<(), SpeechError> {
    let values = t.flatten_all().and_then(|f| f.to_dtype(candle_core::DType::F32)).and_then(|f| f.to_vec1::<f32>()).map_err(merr)?;
    ensure_finite(&values, what)
}

/// Convert the internal tick-based transcript to the public `f64`-seconds transcript.
fn to_public(t: &Transcript) -> crate::Transcript {
    crate::Transcript {
        language: t.language.clone(),
        source: t.source.clone(),
        words: t
            .words
            .iter()
            .map(|w| crate::Word {
                start_sec: w.start.seconds(),
                end_sec: w.end.seconds(),
                text: w.text.clone(),
                // internal speaker index (Option<u32>) → public u8, saturating (never wraps).
                speaker: w.speaker.unwrap_or(0).min(u8::MAX as u32) as u8,
            })
            .collect(),
    }
}

pub struct Whisper {
    id: String,
    model: Mutex<Model>,
    tok: Tokenizer,
    alignment_heads: Vec<(usize, usize)>,
    suppress: Vec<u32>,
    begin_suppress: Vec<u32>,
}

/// The largest JSON file `load` will read (the real ones are at most 2.5 MB).
const MAX_JSON_BYTES: u64 = 16 << 20;
/// The largest weights file `load` will read (the biggest catalogue model is under 1 GB).
const MAX_WEIGHT_BYTES: u64 = 8 << 30;

/// Read `path`, refusing a file larger than `max` bytes.
fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, SpeechError> {
    use std::io::Read;
    // The file's name, not its path: the folder is the user's own and the message may reach a status line.
    let fail = |why: String| SpeechError::Model(format!("{}: {why}", path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy())));
    let file = std::fs::File::open(path).map_err(|e| fail(e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| fail(e.to_string()))?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n > max) {
        return Err(fail(format!("larger than {max} bytes")));
    }
    Ok(bytes)
}

/// The `(layer, head)` pairs whose cross-attention gives word timing: those the model lists, else
/// the second half of the decoder. A listed pair that does not exist is an error, not a skip. A
/// pair listed twice counts once (the first place it is listed keeps its order), so the result
/// never holds more pairs than the decoder has heads (`layers x heads`) however long the list is.
fn alignment_heads(gen_cfg: &serde_json::Value, cfg: &Config) -> Result<Vec<(usize, usize)>, SpeechError> {
    let index = |v: &serde_json::Value| v.as_u64().map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    let listed: Vec<(usize, usize)> =
        gen_cfg["alignment_heads"].as_array().map(|a| a.iter().filter_map(|p| Some((index(&p[0])?, index(&p[1])?))).collect()).unwrap_or_default();
    if listed.is_empty() {
        return Ok((cfg.decoder_layers / 2..cfg.decoder_layers).flat_map(|l| (0..cfg.decoder_attention_heads).map(move |h| (l, h))).collect());
    }
    if let Some((l, h)) = listed.iter().find(|&&(l, h)| l >= cfg.decoder_layers || h >= cfg.decoder_attention_heads) {
        return Err(SpeechError::Model(format!(
            "generation_config.json: alignment head ({l}, {h}) does not exist in a decoder of {} layers with {} heads",
            cfg.decoder_layers, cfg.decoder_attention_heads
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<(usize, usize)> = listed.into_iter().filter(|pair| seen.insert(*pair)).collect();
    // Every pair exists and is distinct, so this cannot fail; it states the bound the rest relies on.
    if unique.len() > cfg.decoder_layers.saturating_mul(cfg.decoder_attention_heads) {
        return Err(SpeechError::Model("generation_config.json: more alignment heads than the decoder has".into()));
    }
    Ok(unique)
}

/// Token ids listed in `gen_cfg[key]`, ignoring entries that are not ids.
fn token_list(gen_cfg: &serde_json::Value, key: &str) -> Option<Vec<u32>> {
    gen_cfg[key].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().and_then(|v| u32::try_from(v).ok())).collect())
}

impl Whisper {
    /// Load a model directory (`config.json`, `generation_config.json`, `tokenizer.json`,
    /// `model.safetensors`).
    ///
    /// The folder's contents are untrusted: the catalogue's size check says nothing about them. So
    /// everything that can be checked without the weights is checked first (the configuration's
    /// ranges, every token id against the vocabulary, the alignment heads), and only then are the
    /// weights read. A folder that fails any check is an `Err`, never a panic or a huge allocation.
    pub fn load(dir: &Path, id: &str) -> Result<Self, SpeechError> {
        let read = |n: &str| read_capped(&dir.join(n), MAX_JSON_BYTES);
        let cfg: Config = serde_json::from_slice(&read("config.json")?).map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
        cfg.validate().map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
        let gen_cfg: serde_json::Value = serde_json::from_slice(&read("generation_config.json")?).unwrap_or_default();
        let tok = Tokenizer::from_json(&String::from_utf8_lossy(&read("tokenizer.json")?), cfg.vocab_size)?;
        let alignment_heads = alignment_heads(&gen_cfg, &cfg)?;
        let mut suppress = token_list(&gen_cfg, "suppress_tokens").unwrap_or_default();
        suppress.extend(tok.non_text_specials());
        suppress.sort_unstable();
        suppress.dedup();
        let begin_suppress = token_list(&gen_cfg, "begin_suppress_tokens").unwrap_or_else(|| vec![220, tok.eot]);
        // Everything above is validated without the weights, which are read last.
        let weights = read_capped(&dir.join("model.safetensors"), MAX_WEIGHT_BYTES)?;
        let model = Model::load(cfg, weights).map_err(|e| SpeechError::Model(format!("model.safetensors: {e}")))?;
        Ok(Self { id: id.to_string(), model: Mutex::new(model), tok, alignment_heads, suppress, begin_suppress })
    }

    pub fn languages(&self) -> Vec<String> {
        self.tok.languages.iter().map(|l| l.0.clone()).collect()
    }

    fn window(mel: &[f32], n_mels: usize, frames: usize, seek: usize) -> Vec<f32> {
        let mut w = vec![0f32; n_mels * N_FRAMES];
        let end = seek.saturating_add(N_FRAMES).min(frames);
        if seek < end {
            // one mel band per row: `frames` values in the spectrogram, `N_FRAMES` in the window
            for (dst, src) in w.chunks_exact_mut(N_FRAMES).zip(mel.chunks_exact(frames.max(1))) {
                if let (Some(d), Some(s)) = (dst.get_mut(..end - seek), src.get(seek..end)) {
                    d.copy_from_slice(s);
                }
            }
        }
        w
    }

    fn detect_language(&self, m: &mut Model, xa: &Tensor) -> Result<String, SpeechError> {
        m.decoder.reset();
        let (logits, _) = m.decoder.forward(&[self.tok.sot], xa, false, None).map_err(merr)?;
        let l = Model::last_logits(&logits).map_err(merr)?;
        ensure_finite(&l, "language detection")?;
        let score = |id: u32| l.get(id as usize).copied().unwrap_or(f32::NEG_INFINITY);
        let best = self.tok.languages.iter().max_by(|a, b| score(a.1).total_cmp(&score(b.1))).map(|x| x.0.clone());
        Ok(best.unwrap_or_else(|| "en".into()))
    }

    /// Apply the timestamp rules and suppression to `logits` for the next token after `seq`
    /// (sampled tokens of this window, prompt excluded). Token ids are checked against the
    /// vocabulary when the model is loaded, but nothing here indexes by one: ranges are clamped.
    fn constrain(&self, logits: &mut [f32], seq: &[u32], max_ts: Option<u32>) {
        let tb = self.tok.timestamp_begin as usize;
        let eot = self.tok.eot as usize;
        let ninf = f32::NEG_INFINITY;
        for &s in &self.suppress {
            if let Some(v) = logits.get_mut(s as usize) {
                *v = ninf;
            }
        }
        if seq.is_empty() {
            for &s in &self.begin_suppress {
                if let Some(v) = logits.get_mut(s as usize) {
                    *v = ninf;
                }
            }
        }
        let last_ts = seq.last().is_some_and(|&t| self.tok.is_timestamp(t));
        let penult_ts = seq.iter().rev().nth(1).is_none_or(|&t| self.tok.is_timestamp(t));
        if last_ts {
            if penult_ts {
                suppress_range(logits, tb, usize::MAX);
            } else {
                suppress_range(logits, 0, eot);
            }
        }
        if let Some(&last) = seq.iter().rev().find(|&&t| self.tok.is_timestamp(t)) {
            let lim = if last_ts && !penult_ts { last } else { last.saturating_add(1) };
            suppress_range(logits, tb, lim as usize);
        }
        if seq.is_empty() {
            suppress_range(logits, 0, tb);
            if let Some(m) = max_ts {
                suppress_range(logits, tb.saturating_add(m as usize).saturating_add(1), usize::MAX);
            }
        }
        // timestamps win when their total probability beats every text token
        let max = logits.iter().copied().fold(ninf, f32::max);
        if max.is_finite() {
            let lse = |s: &[f32]| -> f32 {
                let m = s.iter().copied().fold(ninf, f32::max);
                if !m.is_finite() {
                    return ninf;
                }
                m + s.iter().map(|v| (v - m).exp()).sum::<f32>().ln()
            };
            let ts = logits.get(tb..).map_or(ninf, lse);
            let best_text = logits.get(..tb).map_or(ninf, |s| s.iter().copied().fold(ninf, f32::max));
            if ts > best_text {
                suppress_range(logits, 0, tb);
            }
        }
    }

    /// Greedy decode of one window. Returns sampled tokens (no prompt, no end-of-text), the mean
    /// log-probability and the no-speech probability.
    fn decode_window(&self, m: &mut Model, xa: &Tensor, prompt: &[u32]) -> Result<(Vec<u32>, f32, f32), SpeechError> {
        m.decoder.reset();
        let mut seq: Vec<u32> = Vec::new();
        let (logits, _) = m.decoder.forward(prompt, xa, true, None).map_err(merr)?;
        // no-speech probability from the start-of-transcript position
        let first = logits.get(0).and_then(|l| l.get(0)).and_then(|l| l.to_vec1::<f32>()).map_err(merr)?;
        ensure_finite(&first, "word choices")?;
        let no_speech = self.tok.no_speech.map(|ns| softmax_at(&first, ns as usize)).unwrap_or(0.0);
        let mut cur = Model::last_logits(&logits).map_err(merr)?;
        ensure_finite(&cur, "word choices")?;
        let mut sum_lp = 0f32;
        for _ in 0..MAX_TOKENS.min(m.cfg.max_target_positions / 2) {
            let lp = log_softmax(&cur);
            self.constrain(&mut cur, &seq, Some(50));
            let next = argmax(&cur);
            sum_lp += lp.get(next as usize).copied().unwrap_or(f32::NEG_INFINITY);
            if next == self.tok.eot {
                break;
            }
            seq.push(next);
            if repeating(&seq, self.tok.timestamp_begin) {
                break;
            }
            let (logits, _) = m.decoder.forward(&[next], xa, true, None).map_err(merr)?;
            cur = Model::last_logits(&logits).map_err(merr)?;
            ensure_finite(&cur, "word choices")?;
        }
        let avg = sum_lp / (seq.len() + 1) as f32;
        Ok((seq, avg, no_speech))
    }

    /// Transcribe mono 16 kHz audio, producing the internal tick-based transcript.
    fn run(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        // Refuse before allocating anything for it (see `crate::MAX_AUDIO_SECS`).
        crate::check_length(audio.len())?;
        let mut m = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let n_mels = m.cfg.num_mel_bins;
        let content_frames = audio.len() / SAMPLES_PER_FRAME;
        // Non-finite samples become silence here, once, so no stage below (mel, level detection,
        // speaker labelling) can be poisoned by a NaN or an infinity in the caller's buffer.
        let padded = crate::mel::sanitized(audio, N_FRAMES * SAMPLES_PER_FRAME);
        let audio = padded.get(..audio.len()).unwrap_or(&padded);
        if !progress(0.0, "Analysing audio") {
            return Err(SpeechError::Cancelled);
        }
        let (frames, mel) = crate::mel::log_mel(&padded, n_mels);
        let mut language = opts.language.clone().filter(|l| !l.is_empty() && l != "auto");
        if language.as_deref().is_some_and(|l| self.tok.multilingual() && self.tok.language_token(l).is_none()) {
            return Err(SpeechError::UnknownLanguage(language.unwrap_or_default()));
        }
        let mut words: Vec<Word> = Vec::new();
        let mut seek = 0usize;
        let mut guard = ProgressGuard::default();
        while seek < content_frames {
            let frac = seek as f32 / content_frames.max(1) as f32;
            if !progress(frac * 0.95, &format!("Transcribing {:.0}%", frac * 100.0)) {
                return Err(SpeechError::Cancelled);
            }
            let seg_frames = N_FRAMES.min(content_frames - seek);
            let win = Self::window(&mel, n_mels, frames, seek);
            let x = Tensor::from_vec(win, (1, n_mels, N_FRAMES), &m.device).map_err(merr)?;
            let xa = m.encoder.forward(&x).map_err(merr)?;
            ensure_tensor_finite(&xa, "audio analysis")?;
            let mut prompt = vec![self.tok.sot];
            if self.tok.multilingual() {
                if language.is_none() {
                    language = Some(self.detect_language(&mut m, &xa)?);
                }
                let lang = language.as_deref().unwrap_or("en");
                prompt.push(self.tok.language_token(lang).unwrap_or(self.tok.sot + 1));
                prompt.push(self.tok.transcribe);
            }
            let (toks, avg_lp, no_speech) = self.decode_window(&mut m, &xa, &prompt)?;
            let skip = no_speech > 0.6 && avg_lp < -1.0;
            // where the next window starts
            let is_ts: Vec<bool> = toks.iter().map(|&t| self.tok.is_timestamp(t)).collect();
            let single_ending = matches!(is_ts.as_slice(), [.., false, true]);
            let last_pair = is_ts.windows(2).rposition(|w| w == [true, true]).map(|i| i + 1);
            let advance = match last_pair {
                Some(i) if !single_ending => {
                    let ts = toks.get(i - 1).copied().unwrap_or(self.tok.timestamp_begin).saturating_sub(self.tok.timestamp_begin);
                    usize::try_from(ts).unwrap_or(usize::MAX).saturating_mul(FRAMES_PER_TS).clamp(1, seg_frames.max(1))
                }
                _ => seg_frames,
            };
            let text: Vec<u32> = toks.iter().copied().filter(|&t| t < self.tok.eot).collect();
            if !skip && !text.is_empty() {
                // the text up to the last complete segment (what `advance` covers)
                let upto = match last_pair {
                    Some(i) if !single_ending => i,
                    _ => toks.len(),
                };
                let text: Vec<u32> = toks.get(..upto).unwrap_or(&toks).iter().copied().filter(|&t| t < self.tok.eot).collect();
                let mut aprompt = prompt.clone();
                aprompt.push(self.tok.no_timestamps);
                let times = align::align(&mut m, &xa, &aprompt, &text, self.tok.eot, &self.alignment_heads, seg_frames).map_err(merr)?;
                let offset = seek.saturating_mul(SAMPLES_PER_FRAME);
                let limit = seek.saturating_add(advance).saturating_mul(SAMPLES_PER_FRAME);
                for (wtext, r) in tokenizer::group_words(&self.tok, &text) {
                    let (Some(first), Some(last)) = (times.get(r.start), r.end.checked_sub(1).and_then(|e| times.get(e))) else { continue };
                    let at = |frames: usize| offset.saturating_add(frames.saturating_mul(SAMPLES_PER_FRAME)).min(limit);
                    let a = at(first.0);
                    let b = at(last.1).max(a);
                    let tick = |n: usize| sample_tick(i64::try_from(n).unwrap_or(i64::MAX));
                    words.push(Word::new(wtext, tick(a), tick(b)));
                }
            }
            guard.note(advance, seg_frames)?;
            seek += advance;
        }
        let mut t = Transcript { language: language.unwrap_or_else(|| "en".into()), source: self.id.clone(), speakers: Vec::new(), words };
        t.normalize();
        crate::vad::tighten_words(audio, &mut t.words);
        if opts.diarize && !t.words.is_empty() {
            if !progress(0.96, "Labelling speakers") {
                return Err(SpeechError::Cancelled);
            }
            let p = crate::diarize::Params { max_speakers: opts.max_speakers, ..Default::default() };
            crate::diarize::diarize(audio, &mut t, &p, &mut |status| progress(0.96, status))?;
        }
        progress(1.0, "Done");
        Ok(t)
    }
}

impl crate::Transcriber for Whisper {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<crate::Transcript, SpeechError> {
        let t = self.run(audio, opts, progress)?;
        Ok(to_public(&t))
    }
}

/// Set `logits[from..to]` to minus infinity, with both ends clamped to the slice.
fn suppress_range(logits: &mut [f32], from: usize, to: usize) {
    let to = to.min(logits.len());
    if let Some(range) = logits.get_mut(from..to) {
        range.iter_mut().for_each(|v| *v = f32::NEG_INFINITY);
    }
}

fn softmax_at(x: &[f32], i: usize) -> f32 {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let s: f32 = x.iter().map(|v| (v - m).exp()).sum();
    x.get(i).map(|v| (v - m).exp() / s).unwrap_or(0.0)
}

fn log_softmax(x: &[f32]) -> Vec<f32> {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lse = m + x.iter().map(|v| (v - m).exp()).sum::<f32>().ln();
    x.iter().map(|v| v - lse).collect()
}

fn argmax(x: &[f32]) -> u32 {
    let mut best = (0usize, f32::NEG_INFINITY);
    for (i, &v) in x.iter().enumerate() {
        if v > best.1 {
            best = (i, v);
        }
    }
    u32::try_from(best.0).unwrap_or(0)
}

/// The smallest advance of the audio window that counts as progress: 0.5 s of mel frames (10 ms each).
const MIN_ADVANCE_FRAMES: usize = 50;
/// How many windows in a row may advance less than [`MIN_ADVANCE_FRAMES`] before decoding stops.
const MAX_STALLED_WINDOWS: u32 = 8;

/// Stops a decode that crawls. Each window normally advances by seconds (to the last complete
/// timestamp pair, or by the whole 30 s); a model whose timestamps are broken could advance by
/// 10 ms per window, and a long recording would then take hours of CPU for nothing. A run of
/// [`MAX_STALLED_WINDOWS`] windows that each advance by less than [`MIN_ADVANCE_FRAMES`] is an
/// error, not a slow success. A window cut short by the end of the audio is not a stall.
#[derive(Default)]
struct ProgressGuard {
    stalled: u32,
}

impl ProgressGuard {
    /// Note that a window of `window_frames` frames (at most 3000, fewer at the end of the audio)
    /// advanced the audio by `advance` frames.
    fn note(&mut self, advance: usize, window_frames: usize) -> Result<(), SpeechError> {
        if advance < MIN_ADVANCE_FRAMES.min(window_frames) {
            self.stalled = self.stalled.saturating_add(1);
        } else {
            self.stalled = 0;
        }
        if self.stalled >= MAX_STALLED_WINDOWS {
            return Err(SpeechError::DecodingStopped(format!(
                "{} windows in a row advanced by less than {} s (the model's timestamps do not move forward)",
                self.stalled,
                MIN_ADVANCE_FRAMES as f64 / 100.0
            )));
        }
        Ok(())
    }
}

/// The tail of `seq` (text tokens) repeats a short pattern many times: a decoding loop.
fn repeating(seq: &[u32], tb: u32) -> bool {
    let text: Vec<u32> = seq.iter().copied().filter(|&t| t < tb).collect();
    for n in 1..=8 {
        let reps = if n == 1 { 12 } else { 5 };
        if text.len() < n * reps {
            continue;
        }
        let Some(tail) = text.get(text.len() - n * reps..) else { continue };
        if (1..reps).all(|r| tail.get(r * n..(r + 1) * n) == tail.get(..n)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decode_that_crawls_is_stopped_with_an_error() {
        // 10 ms per window (the hostile tokenizer's behaviour): stops after MAX_STALLED_WINDOWS.
        let mut g = ProgressGuard::default();
        let mut windows = 0;
        let err = loop {
            windows += 1;
            if let Err(e) = g.note(1, 3000) {
                break e;
            }
            assert!(windows < 1000, "the guard never fired");
        };
        assert_eq!(windows, MAX_STALLED_WINDOWS);
        assert!(matches!(&err, SpeechError::DecodingStopped(why) if why.contains("windows in a row")), "{err:?}");
        assert!(err.to_string().starts_with("decoding stopped: "), "the wording is unchanged: {err}");
    }

    #[test]
    fn a_few_short_windows_are_fine_and_progress_resets_the_count() {
        let mut g = ProgressGuard::default();
        for _ in 0..3 {
            for _ in 0..MAX_STALLED_WINDOWS - 1 {
                assert!(g.note(10, 3000).is_ok(), "one short of the limit");
            }
            // A window that moves on by a second resets the run.
            assert!(g.note(100, 3000).is_ok());
        }
        // A whole 30 s window, and the last window of the audio (whatever its length), never count.
        for _ in 0..100 {
            assert!(g.note(3000, 3000).is_ok());
            assert!(g.note(7, 7).is_ok());
            assert!(g.note(49, 49).is_ok());
        }
        // Exactly the minimum counts as progress; one frame less does not.
        assert_eq!(g.stalled, 0);
        assert!(g.note(MIN_ADVANCE_FRAMES, 3000).is_ok() && g.stalled == 0);
        assert!(g.note(MIN_ADVANCE_FRAMES - 1, 3000).is_ok() && g.stalled == 1);
    }

    fn decoder_config(layers: usize, heads: usize) -> Config {
        serde_json::from_value(serde_json::json!({
            "num_mel_bins": 80, "d_model": 384, "encoder_layers": 4, "encoder_attention_heads": 6,
            "decoder_layers": layers, "decoder_attention_heads": heads,
            "max_source_positions": 1500, "max_target_positions": 448, "vocab_size": 51865
        }))
        .unwrap()
    }

    #[test]
    fn alignment_heads_are_deduplicated_and_bounded() {
        let cfg = decoder_config(4, 6);
        // 630 copies of one head (the file that made the SPV's run slow): one head.
        let dup = serde_json::json!({"alignment_heads": vec![[2, 3]; 630]});
        assert_eq!(alignment_heads(&dup, &cfg).unwrap(), vec![(2, 3)]);
        // Order of first appearance is kept.
        let mixed = serde_json::json!({"alignment_heads": [[3, 0], [2, 2], [3, 0], [2, 2], [3, 2]]});
        assert_eq!(alignment_heads(&mixed, &cfg).unwrap(), vec![(3, 0), (2, 2), (3, 2)]);
        // However many are listed, never more than layers x heads.
        let all: Vec<[usize; 2]> = (0..50).flat_map(|_| (0..4).flat_map(|l| (0..6).map(move |h| [l, h]))).collect();
        let huge = serde_json::json!({"alignment_heads": all});
        assert_eq!(alignment_heads(&huge, &cfg).unwrap().len(), 4 * 6);
        // A head that does not exist is still an error, whatever else is listed.
        assert!(alignment_heads(&serde_json::json!({"alignment_heads": [[2, 3], [4, 0]]}), &cfg).is_err());
        // None listed: the decoder's second half.
        assert_eq!(alignment_heads(&serde_json::json!({}), &cfg).unwrap().len(), 2 * 6);
    }

    #[test]
    fn numbers_that_are_not_numbers_are_an_error_naming_where() {
        assert!(ensure_finite(&[], "x").is_ok());
        assert!(ensure_finite(&[0.0, -1.5, f32::MAX, f32::MIN, f32::MIN_POSITIVE / 2.0], "x").is_ok(), "large and subnormal values are numbers");
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let err = ensure_finite(&[1.0, bad, 2.0], "audio analysis").unwrap_err();
            let SpeechError::Model(text) = &err else { panic!("expected a model error, got {err:?}") };
            assert!(text.starts_with("the speech model produced invalid numbers in its audio analysis"), "{text}");
            assert!(!text.to_lowercase().contains("whisper"), "no third-party name in what people read: {text}");
        }
        // The same for a tensor (what the encoder returns).
        let dev = candle_core::Device::Cpu;
        let fine = Tensor::from_vec(vec![0.5f32, -2.0, 1e30, 0.0], (1, 2, 2), &dev).unwrap();
        assert!(ensure_tensor_finite(&fine, "audio analysis").is_ok());
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let t = Tensor::from_vec(vec![0.5f32, bad, 1.0, 0.0], (1, 2, 2), &dev).unwrap();
            assert!(matches!(ensure_tensor_finite(&t, "audio analysis"), Err(SpeechError::Model(m)) if m.contains("invalid numbers")), "{bad}");
        }
    }

    #[test]
    fn repetition_guard() {
        let tb = 1000;
        assert!(repeating(&[5; 12], tb));
        assert!(!repeating(&[5, 5, 5, 6, 7, 8], tb));
        let mut s = Vec::new();
        for _ in 0..5 {
            s.extend([1, 2, 3]);
        }
        assert!(repeating(&s, tb));
        assert!(!repeating(&[1, 2, 3, 4, 5, 6, 7], tb));
    }
}

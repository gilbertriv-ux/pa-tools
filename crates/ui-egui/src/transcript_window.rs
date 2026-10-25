//! Window ▸ Transcript: a track's speech as text, one line per phrase, each with its timecode and
//! speaker. Click a word to move the playhead there; Shift-click selects a run of words; double-click
//! a speaker to name them; double-click a word to correct its text.
//!
//! The speech model runs on a background thread (`track.transcribe`'s own job, see
//! [`soundcraft_engine::cmd::transcribe_job`]) so the app keeps drawing. Starting the job only takes a
//! snapshot of the session: rendering the track's clips, converting the audio and recognising it all
//! happen on that thread, and a Cancel is noticed between render ranges and conversion stretches too,
//! not only by the recogniser. [`step`] collects its result
//! on every frame, even while the window is hidden, and stores it with `track.set_transcript` like
//! any other edit. Everything else the window does goes through commands too (`transport.locate`,
//! `edit.select`, `track.rename_speaker`).
//!
//! The menu entry and the command palette's Transcribe Track start that same job ([`invoke`]): they
//! open this window on the track and return at once. Only calls that come in over the control
//! channel or MCP run `track.transcribe` synchronously, on the calling thread.
//!
//! A stopped transcription leaves its worker thread running until the next time the recogniser
//! checks in; the window keeps the thread's handle and starts no other job until it has finished
//! (see [`TranscriptState::is_stopping`]), so two recognisers never hold the model in memory at once.
//!
//! Nothing here names the speech model's maker or its id: people read the catalogue's display name
//! ("Speech model, base (multilingual)"), and the folders searched go to the log, not the status line.
//!
//! Time origin: a transcript's word times are samples from the session start, the same origin as
//! markers and clip positions, never relative to a clip or to a render range.

use crate::SoundApp;
use crate::theme::{Tokens, bold, mono, regular};
use egui::{Align, Color32, Key, Label, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use serde_json::{Map, Value, json};
use soundcraft_engine::cmd::{SharedTranscriber, TranscribeOutcome, track_param, transcribe_job, transcribe_job_with_transcriber};
use soundcraft_model::{Session, TrackId, TrackKind, Transcript, TranscriptWord};
use soundcraft_time::{SampleRate, Samples};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// The command the Transcribe Track button runs (on a background thread, so the app stays alive).
const TRANSCRIBE_CMD: &str = "track.transcribe";

/// A pause longer than this starts a new line.
pub const GAP_SECS: f64 = 0.8;
/// A line holds at most this many characters (a longer word still gets a line of its own).
pub const MAX_LINE_CHARS: usize = 64;
/// A line holds at most this many words.
pub const MAX_LINE_WORDS: usize = 10;

const ROW_H: f32 = 22.0;
const TIMECODE_W: f32 = 76.0;
const WORD_SIZE: f32 = 12.0;

/// Seconds from the session start → samples at `sample_rate` (rounded, saturating, NaN → 0).
///
/// The origin is pinned to the session start (sample 0), exactly like markers and clip positions;
/// it is not a clip boundary and not the start of any render range. A negative time stays negative.
pub fn sec_to_samples(sec: f64, sample_rate: SampleRate) -> Samples {
    sample_rate.samples(sec)
}

// ---- lines ---------------------------------------------------------------------------------

/// One line of the transcript: the words `first..end` (indices into the transcript), said by one
/// speaker without a long pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub first: usize,
    /// One past the last word.
    pub end: usize,
    pub speaker: u8,
    /// Start of the first word, in samples from session start.
    pub start: Samples,
    /// Latest end among the words.
    pub stop: Samples,
}

/// Whether `text` closes a sentence (a trailing quote or bracket does not hide the full stop).
fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', ')', ']', '”', '’']).ends_with(['.', '?', '!', '…', '。', '？', '！'])
}

/// Groups `words` into lines. A new line starts when the speaker changes, when the pause since the
/// previous word is longer than [`GAP_SECS`], after a word that ends a sentence (`.` `?` `!`), or when
/// the line would pass [`MAX_LINE_WORDS`] words or [`MAX_LINE_CHARS`] characters.
pub fn segments(words: &[TranscriptWord], sample_rate: SampleRate) -> Vec<Segment> {
    let gap = sec_to_samples(GAP_SECS, sample_rate);
    let mut out: Vec<Segment> = Vec::new();
    // Words and characters (single spaces included) in the line being built.
    let (mut count, mut chars) = (0usize, 0usize);
    for (i, w) in words.iter().enumerate() {
        let w_chars = w.text.chars().count();
        let prev = i.checked_sub(1).and_then(|p| words.get(p));
        let breaks = match (out.last(), prev) {
            (Some(line), Some(prev)) => {
                w.speaker != line.speaker
                    || w.start.saturating_sub(prev.end) > gap
                    || ends_sentence(&prev.text)
                    || count >= MAX_LINE_WORDS
                    || chars.saturating_add(1).saturating_add(w_chars) > MAX_LINE_CHARS
            }
            _ => true,
        };
        if breaks {
            out.push(Segment { first: i, end: i + 1, speaker: w.speaker, start: w.start, stop: w.end });
            (count, chars) = (1, w_chars);
        } else if let Some(line) = out.last_mut() {
            line.end = i + 1;
            line.stop = line.stop.max(w.end);
            count += 1;
            chars = chars.saturating_add(1).saturating_add(w_chars);
        }
    }
    out
}

/// The line the playhead is in, or the one just before it when it sits in a pause.
fn segment_at(lines: &[Segment], pos: Samples) -> Option<usize> {
    lines.partition_point(|s| s.start <= pos).checked_sub(1)
}

fn segment_words<'a>(tr: &'a Transcript, seg: &Segment) -> &'a [TranscriptWord] {
    tr.words.get(seg.first..seg.end).unwrap_or(&[])
}

fn segment_text(words: &[TranscriptWord]) -> String {
    words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
}

/// Which words of a line a search touches. `query_lc` is lowercase and trimmed. Matching runs over
/// the line's text (words joined by single spaces), so a phrase can match across words.
fn match_flags(words: &[TranscriptWord], query_lc: &str) -> Vec<bool> {
    let mut flags = vec![false; words.len()];
    if query_lc.is_empty() {
        return flags;
    }
    let mut text = String::new();
    let mut spans = Vec::with_capacity(words.len());
    for w in words {
        if !text.is_empty() {
            text.push(' ');
        }
        let from = text.len();
        text.push_str(&w.text.to_lowercase());
        spans.push((from, text.len()));
    }
    for (hit, _) in text.match_indices(query_lc) {
        let hit_end = hit.saturating_add(query_lc.len());
        for (flag, &(from, to)) in flags.iter_mut().zip(&spans) {
            if from < hit_end && to > hit {
                *flag = true;
            }
        }
    }
    flags
}

/// Segments and the search filter for one transcript, rebuilt only when the document changes.
struct View {
    key: (TrackId, u64, u32),
    /// The search as last applied: lowercase, trimmed.
    query: String,
    segments: Vec<Segment>,
    /// Indices into `segments` that are listed (all of them, or the lines the search touches).
    rows: Vec<usize>,
}

impl View {
    fn build(key: (TrackId, u64, u32), tr: &Transcript, sample_rate: SampleRate) -> View {
        let segments = segments(&tr.words, sample_rate);
        let rows = (0..segments.len()).collect();
        View { key, query: String::new(), segments, rows }
    }

    fn filter(&mut self, tr: &Transcript, query: &str) {
        let q = query.trim().to_lowercase();
        if q == self.query {
            return;
        }
        self.rows = if q.is_empty() {
            (0..self.segments.len()).collect()
        } else {
            let touched = |seg: &Segment| match_flags(segment_words(tr, seg), &q).contains(&true);
            self.segments.iter().enumerate().filter(|(_, seg)| touched(seg)).map(|(i, _)| i).collect()
        };
        self.query = q;
    }
}

// ---- state ---------------------------------------------------------------------------------

/// The window's state, kept on [`SoundApp`].
pub struct TranscriptState {
    pub open: bool,
    /// The Models folder field: the folder that holds the speech model's folder. Empty (the
    /// default) lets the engine search: `$PA_SPEECH_MODELS_DIR`, then the app bundle, then the
    /// app's own data folder. Only what the user typed is sent as `models_dir`, so it is not
    /// prefilled from the environment (that would turn the first tier into the only one).
    pub models_dir: String,
    /// Keep the line under the playhead in view while playing.
    pub follow: bool,
    pub query: String,
    /// The track picked in the window; `None` follows the Edit selection.
    pub track: Option<TrackId>,
    /// What the window last had to say (progress, results, why nothing happened).
    pub status: String,
    job: Option<Running>,
    /// The worker thread of a transcription that was stopped and has not finished yet. No other
    /// job starts while it is alive.
    stopping: Option<std::thread::JoinHandle<()>>,
    /// A recogniser to use instead of looking for the speech model (see
    /// [`TranscriptState::inject_transcriber`]); `None` in the program.
    transcriber: Option<SharedTranscriber>,
    /// A transcription asked for before there was a frame to start it in (see [`invoke`]).
    pending: Option<TrackId>,
    view: Option<View>,
    shown: Option<TrackId>,
    editing: Option<SpeakerEdit>,
    /// A word whose text is being corrected inline (see [`WordEdit`]); double-clicking a word opens it.
    word_editing: Option<WordEdit>,
    /// The word last plain-clicked: where Shift-click starts a selection.
    anchor: Option<usize>,
    /// The line last scrolled to by Follow.
    followed: Option<usize>,
}

impl Default for TranscriptState {
    fn default() -> Self {
        TranscriptState {
            open: false,
            models_dir: String::new(),
            follow: true,
            query: String::new(),
            track: None,
            status: String::new(),
            job: None,
            stopping: None,
            transcriber: None,
            pending: None,
            view: None,
            shown: None,
            editing: None,
            word_editing: None,
            anchor: None,
            followed: None,
        }
    }
}

impl TranscriptState {
    /// Whether a transcription is running.
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Whether a transcription that was stopped is still winding down on its worker thread. A new
    /// one does not start until it has finished.
    pub fn is_stopping(&self) -> bool {
        self.stopping.as_ref().is_some_and(|thread| !thread.is_finished())
    }

    /// Use `transcriber` for every transcription this window starts, instead of looking for the
    /// speech model on disk. This is the seam for tests of the menu, the command palette and this
    /// window, which prove that a background job ends in a stored transcript without the model being
    /// installed; the app itself never calls it, so in the program every job looks for the model.
    pub fn inject_transcriber(&mut self, transcriber: SharedTranscriber) {
        self.transcriber = Some(transcriber);
    }

    /// Whether jobs use an injected recogniser ([`TranscriptState::inject_transcriber`]).
    pub fn has_injected_transcriber(&self) -> bool {
        self.transcriber.is_some()
    }
}

/// A transcription on its worker thread.
struct Running {
    track: TrackId,
    track_name: String,
    /// When it started, in egui time.
    started: f64,
    fraction: f32,
    stage: String,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Msg>,
    /// The worker thread, kept so that a stop can wait for it (see [`TranscriptState::stopping`]).
    thread: Option<std::thread::JoinHandle<()>>,
}

enum Msg {
    Progress(f32, String),
    Done(Result<TranscribeOutcome, String>),
}

/// A speaker's name being typed into its badge.
struct SpeakerEdit {
    /// The line (index into the segments) whose badge is being edited.
    row: usize,
    speaker: u8,
    text: String,
    /// Take focus and select the text on the first frame.
    focus: bool,
    /// Created this frame, so it cannot have been drawn yet.
    fresh: bool,
    /// Drawn this frame; an editor whose line scrolled away is dropped.
    seen: bool,
}

/// A word's text being corrected inline, in the word's place.
struct WordEdit {
    /// The word's index into the transcript's `words` (not into a line).
    index: usize,
    /// The text being typed.
    text: String,
    /// The word's text when editing began: a commit that matches it dispatches nothing.
    original: String,
    /// Take focus and select the text on the first frame.
    focus: bool,
    /// Created this frame, so it cannot have been drawn yet.
    fresh: bool,
    /// Drawn this frame; an editor whose line scrolled away is dropped.
    seen: bool,
}

/// What the window asks for; applied after drawing, through commands.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Locate(Samples),
    Select { track: TrackId, start: Samples, end: Samples },
    Rename { track: TrackId, speaker: u8, name: String },
    EditWord { track: TrackId, index: usize, text: String },
    Anchor(Option<usize>),
    Transcribe(TrackId),
    Cancel,
}

/// Read-only facts about this frame.
struct Frame<'a> {
    s: &'a Session,
    revision: u64,
    pos: Samples,
    playing: bool,
    now: f64,
}

// ---- UI command ----------------------------------------------------------------------------

/// `window.transcript {value?: bool, track?: id|name}`: shows or hides the window (a `track` shows
/// it, on that track). Returns `{"value": shown}`.
pub fn run(app: &mut SoundApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "window.transcript" {
        return None;
    }
    let track = match track_param(&app.engine, id, p, "track") {
        Ok(t) => t,
        Err(e) => return Some(Err(e.to_string())),
    };
    let st = &mut app.transcript;
    st.open = p.get("value").and_then(Value::as_bool).unwrap_or(track.is_some() || !st.open);
    if let Some(t) = track {
        st.track = Some(t);
    }
    Some(Ok(json!({"value": st.open})))
}

/// The first line of `text`: all a status line shows of an error, whose later lines may be a
/// backtrace or a list of folders.
fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// Says `msg` in the window and the main status line, one line of it. A longer message is logged
/// in full.
fn set_status(app: &mut SoundApp, msg: impl Into<String>) {
    let full = msg.into();
    let shown = first_line(&full).to_string();
    if shown != full.trim() {
        log::warn!("transcription: {full}");
    }
    app.ui.status.clone_from(&shown);
    app.transcript.status = shown;
}

/// `track.transcribe` as the menu and the command palette run it: open this window on the track
/// and start its background job, then return. The synchronous command would hold the whole app for
/// as long as the recognition takes (minutes, for a long track); calls over the control channel
/// or MCP still run it that way, because their caller waits for the answer.
///
/// `p` is the command's params: `track` picks the track (else the window's pick, the Edit
/// selection, or the first audio track).
pub(crate) fn invoke(app: &mut SoundApp, p: &Value) {
    let track = match track_param(&app.engine, TRANSCRIBE_CMD, p, "track") {
        Ok(t) => t,
        Err(e) => return set_status(app, e.to_string()),
    };
    app.transcript.open = true;
    if let Some(t) = track {
        app.transcript.track = Some(t);
    }
    let session = app.engine.session();
    let Some(shown) = resolve(app.transcript.track, &candidates(session), session) else {
        return set_status(app, "No audio track to transcribe");
    };
    match app.docking_context.clone() {
        Some(ctx) => start(app, &ctx, shown),
        // Before the first frame there is no context to repaint: the next frame starts it.
        None => app.transcript.pending = Some(shown),
    }
}

// ---- running the speech model --------------------------------------------------------------

/// The language chosen in Setup ▸ Transcription Settings (`transcription.language=<code>`), if any.
fn language_flag(session: &Session) -> Option<String> {
    session.edit.flags.iter().find_map(|f| f.strip_prefix("transcription.language=")).map(str::to_string)
}

/// The params of the `track.transcribe` job for `track`. `models_dir` is sent only when the user
/// typed a Models folder: an explicit folder is the only place the engine then looks, so an empty
/// field must send none and leave the search (environment, app bundle, app data) to the engine.
fn job_params(track: TrackId, models_dir: &str, language: Option<String>) -> Value {
    let mut params = Map::new();
    params.insert("track".into(), json!(track.0));
    let dir = models_dir.trim();
    if !dir.is_empty() {
        params.insert("models_dir".into(), json!(dir));
    }
    if let Some(language) = language {
        params.insert("language".into(), json!(language));
    }
    Value::Object(params)
}

/// What the window says while a stopped transcription is still winding down.
const STOPPING: &str = "Stopping the previous transcription…";
/// What it says once the stopped transcription's thread has ended.
const STOPPED: &str = "Transcription stopped";

/// Starts transcribing `track` on a worker thread.
fn start(app: &mut SoundApp, ctx: &egui::Context, track: TrackId) {
    if app.transcript.job.is_some() {
        return set_status(app, "A transcription is already running");
    }
    // A stopped job's thread may still be working on the model; wait for it, so that two recognisers
    // never run at once (the button is greyed out for the same reason).
    if app.transcript.is_stopping() {
        return set_status(app, STOPPING);
    }
    let Some(track_name) = app.engine.session().track(track).map(|t| t.name.clone()) else {
        set_status(app, "That track no longer exists");
        return;
    };
    let params = job_params(track, &app.transcript.models_dir, language_flag(app.engine.session()));
    // The ordinary job looks for the speech model when it runs; only a recogniser injected on the
    // state (tests) replaces that.
    let job = match &app.transcript.transcriber {
        Some(transcriber) => transcribe_job_with_transcriber(&app.engine, &params, Arc::clone(transcriber)),
        None => transcribe_job(&app.engine, &params),
    };
    let job = match job {
        Ok(job) => job,
        Err(e) => {
            set_status(app, e.to_string());
            return;
        }
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let cancel = Arc::new(AtomicBool::new(false));
    let (stop, repaint) = (Arc::clone(&cancel), ctx.clone());
    let spawned = std::thread::Builder::new().name("transcribe".into()).spawn(move || {
        let progress_tx = tx.clone();
        let (progress_stop, progress_repaint) = (Arc::clone(&stop), repaint.clone());
        let mut progress = move |fraction: f32, stage: &str| -> bool {
            let _ = progress_tx.send(Msg::Progress(fraction, stage.to_string()));
            progress_repaint.request_repaint();
            !progress_stop.load(Ordering::Relaxed)
        };
        let result = job.run(&mut progress).map_err(|e| e.to_string());
        let _ = tx.send(Msg::Done(result));
        repaint.request_repaint();
    });
    let thread = match spawned {
        Ok(thread) => thread,
        Err(e) => {
            set_status(app, format!("Transcription needs a background thread, and this build cannot start one ({e})"));
            return;
        }
    };
    let started = ctx.input(|i| i.time);
    app.transcript.job =
        Some(Running { track, track_name: track_name.clone(), started, fraction: 0.0, stage: String::new(), cancel, rx, thread: Some(thread) });
    app.transcript.status = format!("Transcribing {track_name}...");
    ctx.request_repaint();
}

/// Stops a running transcription. True when there was one. The worker is told to stop at once; it
/// finishes what it is doing and ends a moment later, and until it has, no other transcription
/// starts ([`TranscriptState::is_stopping`]).
pub(crate) fn cancel(app: &mut SoundApp) -> bool {
    // A request that has not started yet belongs to the session being replaced too.
    app.transcript.pending = None;
    let Some(mut job) = app.transcript.job.take() else { return false };
    job.cancel.store(true, Ordering::Relaxed);
    app.transcript.stopping = job.thread.take().filter(|thread| !thread.is_finished());
    if app.transcript.stopping.is_some() {
        set_status(app, STOPPING);
    } else {
        set_status(app, STOPPED);
    }
    true
}

/// Notices that a stopped transcription's thread has ended: joins it and says so, once.
fn reap_stopped(app: &mut SoundApp, ctx: &egui::Context) {
    let Some(thread) = &app.transcript.stopping else { return };
    if !thread.is_finished() {
        // Nothing wakes the window when the thread ends, so look again shortly.
        ctx.request_repaint_after_secs(0.1);
        return;
    }
    if let Some(thread) = app.transcript.stopping.take()
        && thread.join().is_err()
    {
        log::warn!("transcription: the stopped worker thread panicked");
    }
    // Only replace the message it belongs to: something else may have been said since. The window's
    // own status is the window's to change. The app's main status line is not: a stopped
    // transcription that ends minutes after Recover or Open (which stop it) must not wipe their
    // "Recovered ..." message, so the line is touched only while it still shows this very message.
    if app.transcript.status == STOPPING {
        app.transcript.status = STOPPED.to_string();
    }
    if app.ui.status == STOPPING {
        app.ui.status = STOPPED.to_string();
    }
    ctx.request_repaint();
}

/// Collects the worker's progress and result. Called every frame from `logic`, so a result lands
/// even while the window is hidden.
pub fn step(app: &mut SoundApp, ctx: &egui::Context) {
    reap_stopped(app, ctx);
    if let Some(track) = app.transcript.pending.take() {
        // Collect its answer from the next frame on, not in the frame that starts it.
        return start(app, ctx, track);
    }
    let Some(job) = &mut app.transcript.job else { return };
    let mut done = None;
    loop {
        match job.rx.try_recv() {
            Ok(Msg::Progress(fraction, stage)) => {
                job.fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { job.fraction };
                job.stage = stage;
            }
            Ok(Msg::Done(result)) => {
                done = Some(result);
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                done = Some(Err("Transcription stopped unexpectedly".to_string()));
                break;
            }
        }
    }
    let Some(result) = done else {
        // The elapsed time on the progress bar needs a tick even when no progress arrives.
        ctx.request_repaint_after_secs(0.25);
        return;
    };
    if let Some(job) = app.transcript.job.take() {
        finish(app, &job, result);
    }
}

/// Stores a finished transcription, or says why there is nothing to store.
fn finish(app: &mut SoundApp, job: &Running, result: Result<TranscribeOutcome, String>) {
    let outcome = match result {
        Ok(o) => o,
        // One line in the status; `set_status` logs the rest (a backtrace, say).
        Err(e) => return set_status(app, e),
    };
    let Some(transcript) = outcome.transcript else {
        return set_status(app, nothing_stored(&outcome.result));
    };
    // The session may have changed while the speech model worked.
    if !app.engine.session().track(job.track).is_some_and(|t| t.name == job.track_name) {
        return set_status(app, "Track changed; transcript discarded");
    }
    let words: Vec<Value> = transcript.words.iter().map(|w| json!({"start": w.start, "end": w.end, "text": w.text, "speaker": w.speaker})).collect();
    let count = words.len();
    let params = json!({"track": job.track.0, "words": words, "language": transcript.language, "source": transcript.source});
    match app.run("track.set_transcript", params) {
        Ok(_) => set_status(
            app,
            match count {
                0 => "No speech found".to_string(),
                1 => "Transcribed 1 word".to_string(),
                n => format!("Transcribed {n} words"),
            },
        ),
        Err(e) => set_status(app, format!("Could not store the transcript: {e}")),
    }
}

/// What to tell the user when a finished job carries no transcript.
fn nothing_stored(result: &Value) -> String {
    let status = result.get("status").and_then(Value::as_str).unwrap_or("");
    match status {
        "no_models_dir" | "model_unavailable" => model_unavailable(result),
        "" => "Transcription finished without a transcript".to_string(),
        other => format!("Transcription finished without a transcript (status: {other})"),
    }
}

/// Why the speech model cannot be used, in words a status line can carry. The engine's `error` has
/// the full text, with every folder it searched: that goes to the log. What is shown names the
/// model by its display name (`model_name`), never by its id or maker, and leaves the folders out.
fn model_unavailable(result: &Value) -> String {
    let text = |key: &str| result.get(key).and_then(Value::as_str).map(first_line).filter(|t| !t.is_empty());
    if let Some(error) = result.get("error").and_then(Value::as_str) {
        log::warn!("speech model unavailable: {error}");
    }
    let name = text("model_name").unwrap_or("The speech model");
    let why = text("why").map_or_else(|| format!("{name} is not available"), str::to_string);
    let advice = match text("reason").unwrap_or("not_found") {
        "unsupported_build" => "",
        "invalid_model" | "unknown_model" => " Check the speech model's folder, or choose another Models folder, then try again.",
        _ => MODEL_ADVICE,
    };
    format!("{why}.{advice}")
}

/// What to do when the speech model was not found.
const MODEL_ADVICE: &str =
    " Set the Models folder to the folder that contains it, or leave it empty to use the app's own models folder, then try again.";

/// The Models folder field: its hint while empty, and its tooltip. The model's own folder name is a
/// file-system detail and stays out of them, as does the model's maker.
const MODELS_FOLDER_HINT: &str = "App's own models folder";
const MODELS_FOLDER_TIP: &str = "The folder that contains the speech model's folder. Leave it empty to use the app's own models folder.";

/// What the tooltip over a transcript's summary says about where it came from.
fn made_with(source: &str) -> String {
    format!("Made with {}", soundcraft_engine::cmd::source_label(source))
}

// ---- actions -------------------------------------------------------------------------------

/// Names a speaker in a track's transcript (`track.rename_speaker`); an empty name resets it.
pub(crate) fn rename_speaker(app: &mut SoundApp, track: TrackId, speaker: u8, name: &str) -> bool {
    app.run("track.rename_speaker", json!({"track": track.0, "speaker": speaker, "name": name})).is_ok()
}

/// Corrects the text of one word in a track's transcript (`track.edit_word`). The command trims and
/// caps the text and no-ops an empty or unchanged value.
pub(crate) fn edit_word(app: &mut SoundApp, track: TrackId, index: usize, text: &str) -> bool {
    app.run("track.edit_word", json!({"track": track.0, "index": index, "text": text})).is_ok()
}

/// What clicking word `index` does: move the playhead to its start; with Shift also select the
/// words from the last plain-clicked one (else just this one).
fn click_actions(track: TrackId, words: &[TranscriptWord], index: usize, anchor: Option<usize>, shift: bool) -> Vec<Action> {
    let Some(word) = words.get(index) else { return Vec::new() };
    let mut actions = vec![Action::Locate(word.start)];
    if shift {
        let from = anchor.filter(|a| *a < words.len()).unwrap_or(index);
        let (lo, hi) = (from.min(index), from.max(index));
        let start = words.get(lo).map_or(word.start, |w| w.start);
        let end = words.get(lo..=hi).unwrap_or(&[]).iter().map(|w| w.end).max().unwrap_or(word.end);
        actions.push(Action::Select { track, start, end: end.max(start) });
        actions.push(Action::Anchor(Some(from)));
    } else {
        actions.push(Action::Anchor(Some(index)));
    }
    actions
}

fn apply(app: &mut SoundApp, ctx: &egui::Context, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Locate(at) => {
                let _ = app.run("transport.locate", json!({"at": at}));
            }
            Action::Select { track, start, end } => {
                let _ = app.run("edit.select", json!({"tracks": [track.0], "start": start, "end": end}));
            }
            Action::Rename { track, speaker, name } => {
                let _ = rename_speaker(app, track, speaker, &name);
            }
            Action::EditWord { track, index, text } => {
                let _ = edit_word(app, track, index, &text);
            }
            Action::Anchor(a) => app.transcript.anchor = a,
            Action::Transcribe(track) => start(app, ctx, track),
            Action::Cancel => {
                cancel(app);
            }
        }
    }
}

// ---- drawing -------------------------------------------------------------------------------

/// Tracks that can be shown: audio tracks, and any other track that already has a transcript.
fn candidates(s: &Session) -> Vec<(TrackId, String)> {
    s.tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Master && (t.kind == TrackKind::Audio || s.transcript(t.id).is_some()))
        .map(|t| (t.id, t.name.clone()))
        .collect()
}

/// The track to show: the one picked in the window, else the Edit selection's first track, else the
/// first audio track; always one of `choices`.
fn resolve(picked: Option<TrackId>, choices: &[(TrackId, String)], s: &Session) -> Option<TrackId> {
    let listed = |id: &TrackId| choices.iter().any(|(c, _)| c == id);
    picked
        .filter(listed)
        .or_else(|| s.edit.selected_tracks.first().copied().filter(listed))
        .or_else(|| s.tracks.iter().find(|t| t.kind == TrackKind::Audio).map(|t| t.id).filter(listed))
        .or_else(|| choices.first().map(|(id, _)| *id))
}

const SPEAKER_COLORS: [[u8; 3]; 6] = [[90, 150, 230], [230, 150, 70], [110, 190, 110], [200, 110, 190], [220, 190, 70], [80, 190, 200]];

fn speaker_color(speaker: u8) -> [u8; 3] {
    SPEAKER_COLORS.get(usize::from(speaker) % SPEAKER_COLORS.len()).copied().unwrap_or([150, 150, 150])
}

fn elapsed_text(secs: f64) -> String {
    let whole = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// The window. Does nothing while it is closed.
pub fn show(app: &mut SoundApp, ctx: &egui::Context) {
    if !app.transcript.open {
        return;
    }
    let pos = app.position();
    let playing = app.is_playing();
    let revision = app.engine.revision;
    let now = ctx.input(|i| i.time);
    let mut open = true;
    let mut actions = Vec::new();
    {
        let st = &mut app.transcript;
        let f = Frame { s: app.engine.session(), revision, pos, playing, now };
        egui::Window::new(RichText::new("Transcript").font(bold(13.0)))
            .open(&mut open)
            // First opening: right side, below the toolbar, so it never covers the menu bar
            // or the transport (like the floating panels in panels.rs).
            .default_pos(egui::pos2((ctx.content_rect().width() - 640.0).max(16.0), 130.0))
            .default_size(vec2(620.0, 520.0))
            .min_size(vec2(400.0, 260.0))
            .resizable(true)
            .show(ctx, |ui| body(ui, st, &f, &mut actions));
    }
    app.transcript.open = open;
    apply(app, ctx, actions);
}

fn body(ui: &mut Ui, st: &mut TranscriptState, f: &Frame, out: &mut Vec<Action>) {
    let t = Tokens::current();
    let choices = candidates(f.s);
    let shown = resolve(st.track, &choices, f.s);
    let running = st.job.is_some();
    // A stopped job that is still winding down keeps the button off, too.
    let busy = running || st.is_stopping();

    if let Some(shown) = shown {
        let shown_name = choices.iter().find(|(id, _)| *id == shown).map(|(_, n)| n.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Track").font(regular(12.0)).color(t.text_dim));
            egui::ComboBox::from_id_salt("transcript_track").selected_text(shown_name).width(190.0).show_ui(ui, |ui| {
                for (id, name) in &choices {
                    if ui.selectable_label(*id == shown, name).clicked() {
                        st.track = Some(*id);
                    }
                }
            });
            let button = ui.add_enabled(!busy, egui::Button::new("Transcribe Track"));
            if button
                .on_hover_text(format!("Turns this track's speech into text ({TRANSCRIBE_CMD}). It runs in the background, so you can keep working."))
                .clicked()
            {
                out.push(Action::Transcribe(shown));
            }
            if running && ui.button("Cancel").clicked() {
                out.push(Action::Cancel);
            }
        });
    } else {
        ui.label(RichText::new("No audio tracks to transcribe.").font(regular(12.0)).color(t.text_dim));
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("Models folder").font(regular(12.0)).color(t.text_dim));
        ui.add(egui::TextEdit::singleline(&mut st.models_dir).hint_text(MODELS_FOLDER_HINT).desired_width(f32::INFINITY))
            .on_hover_text(MODELS_FOLDER_TIP);
    });

    if let Some(job) = &st.job {
        let elapsed = elapsed_text(f.now - job.started);
        let mut label = format!("{:.0}%", job.fraction * 100.0);
        if !job.stage.is_empty() {
            label.push_str(&format!("  {}", job.stage));
        }
        label.push_str(&format!("  {elapsed}"));
        ui.add(egui::ProgressBar::new(job.fraction).text(RichText::new(label).font(regular(11.0))).desired_width(ui.available_width()));
    }
    if !st.status.is_empty() {
        ui.label(RichText::new(&st.status).font(regular(11.0)).color(t.text_dim));
    }
    ui.separator();

    let Some(shown) = shown else { return };
    if st.shown != Some(shown) {
        st.shown = Some(shown);
        (st.anchor, st.followed, st.editing, st.word_editing) = (None, None, None, None);
    }
    let Some(tr) = f.s.transcript(shown) else {
        let msg = "No transcript yet. Choose Transcribe Track to turn this track's speech into text.";
        ui.label(RichText::new(msg).font(regular(12.0)).color(t.text_dim));
        return;
    };
    if tr.words.is_empty() {
        ui.label(RichText::new("No speech found.").font(regular(12.0)).color(t.text_dim));
        return;
    }
    transcript_view(ui, st, f, tr, shown, out);
}

/// The meta line, search and the list of lines for a transcript that has words.
fn transcript_view(ui: &mut Ui, st: &mut TranscriptState, f: &Frame, tr: &Transcript, track: TrackId, out: &mut Vec<Action>) {
    let t = Tokens::current();
    let key = (track, f.revision, f.s.sample_rate.hz());
    let mut view = match st.view.take() {
        Some(v) if v.key == key => v,
        _ => View::build(key, tr, f.s.sample_rate),
    };

    ui.horizontal_wrapped(|ui| {
        let words = tr.words.len();
        let language = if tr.language.is_empty() { "language unknown" } else { tr.language.as_str() };
        let meta =
            ui.label(RichText::new(format!("{words} {} · {language}", if words == 1 { "word" } else { "words" })).font(regular(12.0)).color(t.text));
        if !tr.source.is_empty() {
            meta.on_hover_text(made_with(&tr.source));
        }
        if ui.add_enabled(!view.rows.is_empty(), egui::Button::new("Copy")).on_hover_text("Copy the listed lines as time, speaker and text").clicked()
        {
            ui.ctx().copy_text(copy_text(f.s, tr, &view));
            st.status = format!("Copied {} lines", view.rows.len());
        }
        ui.add(egui::TextEdit::singleline(&mut st.query).hint_text("Search").desired_width(150.0))
            .on_hover_text("Show only the lines that contain this text (not case-sensitive)");
        crate::widgets::checkbox(ui, &mut st.follow, "Follow playhead");
    });
    view.filter(tr, &st.query);

    if view.rows.is_empty() {
        let msg = format!("No lines contain \"{}\".", st.query.trim());
        ui.label(RichText::new(msg).font(regular(12.0)).color(t.text_dim));
        st.view = Some(view);
        return;
    }

    let current = segment_at(&view.segments, f.pos);
    let highlighted = current.filter(|c| view.segments.get(*c).is_some_and(|s| f.pos <= s.stop));
    // Follow: scroll once when the playhead enters a new line.
    let follow_row = match (st.follow && f.playing, current) {
        (true, Some(c)) if st.followed != Some(c) => view.rows.partition_point(|r| *r <= c).checked_sub(1).map(|row| (c, row)),
        _ => None,
    };
    if !f.playing {
        st.followed = None;
    }
    let shift = ui.input(|i| i.modifiers.shift);

    let rhs = ROW_H + ui.spacing().item_spacing.y;
    let rows = &view.rows;
    let segs = &view.segments;
    let query = view.query.as_str();
    let (editing, word_editing, anchor, followed) = (&mut st.editing, &mut st.word_editing, st.anchor, &mut st.followed);
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        if let Some((line, row)) = follow_row {
            let top = ui.max_rect().top() - range.start as f32 * rhs;
            let target = Rect::from_min_size(pos2(ui.max_rect().left(), top + row as f32 * rhs), vec2(ui.max_rect().width(), ROW_H));
            if !ui.clip_rect().contains_rect(target) {
                ui.scroll_to_rect(target, Some(Align::Center));
            }
            *followed = Some(line);
        }
        for row in range {
            let Some((seg_i, seg)) = rows.get(row).and_then(|i| segs.get(*i).map(|s| (*i, s))) else { continue };
            let env = RowEnv { s: f.s, tr, track, query, shift, anchor, current: highlighted == Some(seg_i) };
            draw_row(ui, &env, seg_i, seg, editing, word_editing, out);
        }
    });
    // An editor whose line scrolled out of view cannot be typed into: drop it.
    if let Some(ed) = &mut st.editing {
        if ed.seen || ed.fresh {
            (ed.seen, ed.fresh) = (false, false);
        } else {
            st.editing = None;
        }
    }
    if let Some(ed) = &mut st.word_editing {
        if ed.seen || ed.fresh {
            (ed.seen, ed.fresh) = (false, false);
        } else {
            st.word_editing = None;
        }
    }
    st.view = Some(view);
}

/// "time  speaker  text" for each listed line.
fn copy_text(s: &Session, tr: &Transcript, view: &View) -> String {
    let mut text = String::new();
    for seg in view.rows.iter().filter_map(|i| view.segments.get(*i)) {
        let time = s.format_position(seg.start, s.edit.main_counter);
        text.push_str(&format!("{time}  {}  {}\n", tr.speaker_label(seg.speaker), segment_text(segment_words(tr, seg))));
    }
    text
}

struct RowEnv<'a> {
    s: &'a Session,
    tr: &'a Transcript,
    track: TrackId,
    /// The search, lowercase and trimmed.
    query: &'a str,
    shift: bool,
    anchor: Option<usize>,
    /// The playhead is in this line.
    current: bool,
}

fn draw_row(
    ui: &mut Ui,
    env: &RowEnv,
    seg_i: usize,
    seg: &Segment,
    editing: &mut Option<SpeakerEdit>,
    word_editing: &mut Option<WordEdit>,
    out: &mut Vec<Action>,
) {
    let t = Tokens::current();
    let words = segment_words(env.tr, seg);
    let hits = match_flags(words, env.query);
    ui.push_id(("transcript_line", seg_i), |ui| {
        let backdrop = ui.painter().add(Shape::Noop);
        let line = ui.horizontal(|ui| {
            ui.set_height(ROW_H);
            ui.spacing_mut().item_spacing.x = 6.0;

            // Timecode (in the user's main counter format); clicking it goes to the start of the line.
            let time = env.s.format_position(seg.start, env.s.edit.main_counter);
            let galley = ui.painter().layout_no_wrap(time, mono(11.0), t.text_dim);
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x.max(TIMECODE_W), ROW_H), Sense::click());
            ui.painter().galley(pos2(rect.min.x, rect.center().y - galley.size().y / 2.0), galley, t.text_dim);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Go to the start of this line").clicked() {
                out.push(Action::Locate(seg.start));
            }

            // Speaker badge; double-click to rename.
            let editing_here = editing.as_ref().is_some_and(|e| e.row == seg_i);
            if editing_here {
                speaker_editor(ui, env, editing, out);
            } else {
                let [r, g, b] = speaker_color(seg.speaker);
                let badge = egui::Button::new(RichText::new(env.tr.speaker_label(seg.speaker)).font(bold(10.5)).color(t.text))
                    .fill(Color32::from_rgba_unmultiplied(r, g, b, 64))
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(r, g, b, 190)))
                    .corner_radius(9.0)
                    .min_size(vec2(62.0, 18.0));
                let badge = ui.add(badge).on_hover_text("Double-click to name this speaker");
                if badge.double_clicked() {
                    *editing = Some(SpeakerEdit {
                        row: seg_i,
                        speaker: seg.speaker,
                        text: env.tr.speaker_label(seg.speaker),
                        focus: true,
                        fresh: true,
                        seen: false,
                    });
                }
            }

            // The words, each clickable; double-click one to correct its text in place.
            ui.spacing_mut().item_spacing.x = 4.0;
            for (k, w) in words.iter().enumerate() {
                let word_index = seg.first + k;
                if word_editing.as_ref().is_some_and(|e| e.index == word_index) {
                    word_editor(ui, env, word_editing, out);
                    continue;
                }
                let hit = hits.get(k).copied().unwrap_or(false);
                let mut text = RichText::new(&w.text).font(regular(WORD_SIZE)).color(t.text);
                if hit {
                    text = text.background_color(t.accent.gamma_multiply(0.5));
                }
                let resp = ui.add(Label::new(text).sense(Sense::click()).selectable(false).extend());
                if resp.hovered() {
                    ui.painter().rect_filled(resp.rect.expand2(vec2(2.0, 0.0)), 3.0, t.accent.gamma_multiply(0.22));
                }
                let full = || segment_text(words);
                let resp =
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Double-click to correct this word").on_hover_ui(|ui| {
                        ui.set_max_width(420.0);
                        ui.label(full());
                    });
                // A double-click opens the inline editor; a plain or Shift-click still seeks or selects.
                if resp.double_clicked() {
                    *word_editing =
                        Some(WordEdit { index: word_index, text: w.text.clone(), original: w.text.clone(), focus: true, fresh: true, seen: false });
                } else if resp.clicked() {
                    out.extend(click_actions(env.track, &env.tr.words, word_index, env.anchor, env.shift));
                }
            }
        });
        if env.current {
            let full = Rect::from_min_size(line.response.rect.min, vec2(ui.max_rect().width(), ROW_H));
            ui.painter().set(backdrop, Shape::rect_filled(full, 3.0, t.accent.gamma_multiply(0.16)));
        }
    });
}

/// The inline name field in a speaker badge's place: Enter (or clicking away) keeps the name, Esc
/// drops it.
fn speaker_editor(ui: &mut Ui, env: &RowEnv, editing: &mut Option<SpeakerEdit>, out: &mut Vec<Action>) {
    let Some(ed) = editing.as_mut() else { return };
    let id = ui.id().with("speaker_name");
    let field = egui::TextEdit::singleline(&mut ed.text).id(id).font(bold(10.5)).char_limit(Transcript::MAX_SPEAKER_NAME_CHARS).desired_width(110.0);
    let resp = ui.add(field);
    ed.seen = true;
    if ed.focus {
        ed.focus = false;
        resp.request_focus();
        let end = egui::text::CCursor::new(ed.text.chars().count());
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
        egui::TextEdit::store_state(ui.ctx(), id, state);
    }
    let escaped = ui.input(|i| i.key_pressed(Key::Escape));
    if escaped {
        *editing = None;
    } else if resp.lost_focus() {
        // Keeping the default label as it is means "no name".
        let default_label = format!("Speaker {}", ed.speaker);
        let name = if ed.text.trim() == default_label { String::new() } else { ed.text.clone() };
        out.push(Action::Rename { track: env.track, speaker: ed.speaker, name });
        *editing = None;
    }
}

/// The inline word field in a word's place: Enter (or clicking away) keeps the correction, Esc drops
/// it. An empty or unchanged value dispatches nothing — the word is left as it was.
fn word_editor(ui: &mut Ui, env: &RowEnv, word_editing: &mut Option<WordEdit>, out: &mut Vec<Action>) {
    let Some(ed) = word_editing.as_mut() else { return };
    let id = ui.id().with(("word_edit", ed.index));
    // Wide enough for the current text, so correcting a word does not jump the layout about.
    let width = (ed.text.chars().count() as f32 * WORD_SIZE * 0.62 + 16.0).clamp(48.0, 240.0);
    let field = egui::TextEdit::singleline(&mut ed.text).id(id).font(regular(WORD_SIZE)).char_limit(128).desired_width(width);
    let resp = ui.add(field);
    ed.seen = true;
    if ed.focus {
        ed.focus = false;
        resp.request_focus();
        let end = egui::text::CCursor::new(ed.text.chars().count());
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
        egui::TextEdit::store_state(ui.ctx(), id, state);
    }
    let escaped = ui.input(|i| i.key_pressed(Key::Escape));
    if escaped {
        *word_editing = None;
    } else if resp.lost_focus() {
        // Normalise as the command would, so an unchanged or blank value dispatches nothing.
        let normalized: String = ed.text.trim().chars().take(128).collect();
        if !normalized.is_empty() && normalized != ed.original {
            out.push(Action::EditWord { track: env.track, index: ed.index, text: ed.text.clone() });
        }
        *word_editing = None;
    }
}

// ---- tests ---------------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn rate() -> SampleRate {
        SampleRate::new(48_000).unwrap()
    }

    /// A word of `text` starting at `at_ms`, lasting `len_ms`.
    fn word(text: &str, at_ms: i64, len_ms: i64, speaker: u8) -> TranscriptWord {
        TranscriptWord { start: at_ms * 48, end: (at_ms + len_ms) * 48, text: text.into(), speaker }
    }

    fn lines(words: &[TranscriptWord]) -> Vec<(usize, usize)> {
        segments(words, rate()).iter().map(|s| (s.first, s.end)).collect()
    }

    #[test]
    fn seconds_become_samples_from_the_session_start() {
        assert_eq!(sec_to_samples(1.0, rate()), 48_000);
        assert_eq!(sec_to_samples(0.5, rate()), 24_000);
        assert_eq!(sec_to_samples(0.0, rate()), 0);
        assert_eq!(sec_to_samples(1.0, SampleRate::new(44_100).unwrap()), 44_100);
        assert_eq!(sec_to_samples(-1.0, rate()), -48_000, "a time before the session start stays before it");
        assert_eq!(sec_to_samples(f64::NAN, rate()), 0);
        assert_eq!(sec_to_samples(f64::INFINITY, rate()), i64::MAX);
        assert_eq!(sec_to_samples(f64::NEG_INFINITY, rate()), i64::MIN);
        // Rounds to the nearest sample.
        assert_eq!(sec_to_samples(1.0 / 48_000.0 * 2.4, rate()), 2);
        assert_eq!(sec_to_samples(1.0 / 48_000.0 * 2.6, rate()), 3);
    }

    #[test]
    fn no_words_make_no_lines() {
        assert!(segments(&[], rate()).is_empty());
    }

    #[test]
    fn a_speaker_change_starts_a_new_line() {
        let w = [word("hi", 0, 200, 0), word("there", 250, 200, 0), word("hello", 500, 200, 1), word("you", 750, 200, 1)];
        assert_eq!(lines(&w), vec![(0, 2), (2, 4)]);
        assert_eq!(segments(&w, rate()).iter().map(|s| s.speaker).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn a_pause_longer_than_eight_tenths_of_a_second_starts_a_new_line() {
        // 200 ms word, then exactly 800 ms of silence: still one line.
        let same = [word("a", 0, 200, 0), word("b", 1_000, 200, 0)];
        assert_eq!(lines(&same), vec![(0, 2)]);
        // One more millisecond: a new line.
        let split = [word("a", 0, 200, 0), word("b", 1_001, 200, 0)];
        assert_eq!(lines(&split), vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn a_sentence_end_starts_a_new_line() {
        for end in [".", "?", "!", "...", ".\"", "!)", "…"] {
            let w = [word(&format!("done{end}"), 0, 200, 0), word("next", 250, 200, 0)];
            assert_eq!(lines(&w), vec![(0, 1), (1, 2)], "{end}");
        }
        let w = [word("comma,", 0, 200, 0), word("goes", 250, 200, 0), word("on", 500, 200, 0)];
        assert_eq!(lines(&w), vec![(0, 3)], "a comma does not end a line");
    }

    #[test]
    fn a_line_holds_at_most_ten_words() {
        let w: Vec<_> = (0..25).map(|i| word("ab", i * 100, 80, 0)).collect();
        assert_eq!(lines(&w), vec![(0, 10), (10, 20), (20, 25)]);
    }

    #[test]
    fn a_line_holds_at_most_sixty_four_characters() {
        // Ten-letter words: 6 fit in 6*10 + 5 spaces = 65? No: 64 allows 5 (5*10 + 4 = 54; a sixth makes 65).
        let w: Vec<_> = (0..12).map(|i| word("abcdefghij", i * 100, 80, 0)).collect();
        assert_eq!(lines(&w), vec![(0, 5), (5, 10), (10, 12)]);
        // Exactly 64 characters still fits: 9 + 1 + 54 = 64.
        let exact = [word("abcdefghi", 0, 80, 0), word(&"x".repeat(54), 100, 80, 0)];
        assert_eq!(lines(&exact), vec![(0, 2)]);
        // One more does not.
        let over = [word("abcdefghi", 0, 80, 0), word(&"x".repeat(55), 100, 80, 0)];
        assert_eq!(lines(&over), vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn a_word_longer_than_a_line_gets_a_line_of_its_own() {
        let w = [word(&"y".repeat(100), 0, 80, 0), word("ok", 100, 80, 0)];
        assert_eq!(lines(&w), vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn every_word_lands_in_exactly_one_line_in_order() {
        let w: Vec<_> = (0..500).map(|i| word(if i % 7 == 0 { "end." } else { "word" }, i * 150 + (i / 40) * 900, 100, (i / 33 % 3) as u8)).collect();
        let segs = segments(&w, rate());
        let mut next = 0;
        for s in &segs {
            assert_eq!(s.first, next);
            assert!(s.end > s.first && s.end <= w.len());
            assert!(w[s.first..s.end].iter().all(|x| x.speaker == s.speaker));
            assert_eq!(s.start, w[s.first].start);
            next = s.end;
        }
        assert_eq!(next, w.len());
    }

    #[test]
    fn a_lines_stop_is_its_latest_end() {
        // The second word ends before the first (overlapping speech).
        let w = [word("long", 0, 600, 0), word("short", 100, 100, 0)];
        let segs = segments(&w, rate());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].stop, 600 * 48);
    }

    #[test]
    fn the_playhead_finds_its_line() {
        let w = [word("a", 0, 200, 0), word("b", 250, 200, 1), word("c", 5_000, 200, 1)];
        let segs = segments(&w, rate());
        assert_eq!(segment_at(&segs, -5), None, "before the first word");
        assert_eq!(segment_at(&segs, 0), Some(0));
        assert_eq!(segment_at(&segs, 260 * 48), Some(1));
        assert_eq!(segment_at(&segs, 3_000 * 48), Some(1), "in a pause: the line just before");
        assert_eq!(segment_at(&segs, 6_000 * 48), Some(2));
        assert_eq!(segment_at(&[], 10), None);
    }

    #[test]
    fn search_matches_words_and_phrases_without_regard_to_case() {
        let w = [word("Good", 0, 100, 0), word("MORNING,", 150, 100, 0), word("everyone", 300, 100, 0)];
        assert_eq!(match_flags(&w, "morn"), vec![false, true, false]);
        assert_eq!(match_flags(&w, "good morning"), vec![true, true, false], "a phrase spans words");
        assert_eq!(match_flags(&w, "one"), vec![false, false, true]);
        assert_eq!(match_flags(&w, "zzz"), vec![false; 3]);
        assert_eq!(match_flags(&w, ""), vec![false; 3]);
        assert_eq!(match_flags(&[], "x"), Vec::<bool>::new());
        // Characters whose lowercase form is longer or shorter must not break the offsets.
        let odd = [word("İstanbul", 0, 100, 0), word("Straße", 150, 100, 0)];
        assert_eq!(match_flags(&odd, "stra"), vec![false, true]);
    }

    #[test]
    fn the_filter_keeps_only_matching_lines() {
        let tr = Transcript {
            track: TrackId(1),
            words: vec![
                word("alpha", 0, 100, 0),
                word("beta.", 150, 100, 0),
                word("gamma", 300, 100, 1),
                word("delta", 450, 100, 1),
                word("ALPHA", 5_000, 100, 0),
            ],
            language: "en".into(),
            source: String::new(),
            speaker_names: Vec::new(),
        };
        let mut view = View::build((TrackId(1), 1, 48_000), &tr, rate());
        assert_eq!(view.rows.len(), 3);
        view.filter(&tr, "  Alpha ");
        assert_eq!(view.rows, vec![0, 2]);
        view.filter(&tr, "delta");
        assert_eq!(view.rows, vec![1]);
        view.filter(&tr, "");
        assert_eq!(view.rows, vec![0, 1, 2]);
    }

    #[test]
    fn a_click_goes_to_the_word_and_shift_selects_a_run() {
        let w: Vec<_> = (0..6).map(|i| word("w", i * 300, 200, 0)).collect();
        let track = TrackId(3);
        assert_eq!(click_actions(track, &w, 2, None, false), vec![Action::Locate(600 * 48), Action::Anchor(Some(2))]);
        // Shift without an anchor selects just that word.
        assert_eq!(
            click_actions(track, &w, 2, None, true),
            vec![Action::Locate(600 * 48), Action::Select { track, start: 600 * 48, end: 800 * 48 }, Action::Anchor(Some(2))]
        );
        // Shift after a plain click selects from the anchor, whichever way round.
        let forward = click_actions(track, &w, 4, Some(1), true);
        assert_eq!(forward[1], Action::Select { track, start: 300 * 48, end: 1_400 * 48 });
        let backward = click_actions(track, &w, 1, Some(4), true);
        assert_eq!(backward[0], Action::Locate(300 * 48));
        assert_eq!(backward[1], Action::Select { track, start: 300 * 48, end: 1_400 * 48 });
        // An anchor from an older transcript is ignored; an unknown word does nothing.
        assert_eq!(click_actions(track, &w, 2, Some(99), true)[1], Action::Select { track, start: 600 * 48, end: 800 * 48 });
        assert!(click_actions(track, &w, 99, None, false).is_empty());
    }

    #[test]
    fn the_track_shown_is_picked_then_selected_then_the_first_audio_track() {
        let mut s = Session::default();
        let a = s.add_track(TrackKind::Audio, soundcraft_model::ChannelFormat::Mono, Some("A"));
        let b = s.add_track(TrackKind::Audio, soundcraft_model::ChannelFormat::Mono, Some("B"));
        let choices = candidates(&s);
        assert!(choices.iter().all(|(id, _)| *id == a || *id == b) && choices.len() == 2, "no master in the list: {choices:?}");
        assert_eq!(resolve(None, &choices, &s), Some(a));
        s.edit.selected_tracks = vec![b];
        assert_eq!(resolve(None, &choices, &s), Some(b), "follows the Edit selection");
        assert_eq!(resolve(Some(a), &choices, &s), Some(a), "a pick wins");
        assert_eq!(resolve(Some(TrackId(999)), &choices, &s), Some(b), "a pick that is gone falls back");
        assert_eq!(resolve(None, &[], &s), None);
    }

    #[test]
    fn a_track_that_is_not_audio_is_listed_once_it_has_a_transcript() {
        let mut s = Session::default();
        s.add_track(TrackKind::Audio, soundcraft_model::ChannelFormat::Mono, Some("Voice"));
        let midi = s.add_track(TrackKind::Midi, soundcraft_model::ChannelFormat::Mono, Some("Keys"));
        assert!(!candidates(&s).iter().any(|(id, _)| *id == midi));
        s.set_transcript(Transcript { track: midi, words: Vec::new(), language: String::new(), source: String::new(), speaker_names: Vec::new() });
        assert!(candidates(&s).iter().any(|(id, _)| *id == midi));
    }

    /// What the engine's `model_unavailable` outcome carries when the model is not on disk.
    fn not_found() -> Value {
        json!({
            "status": "model_unavailable", "model": "whisper-base", "model_name": "Speech model, base (multilingual)", "reason": "not_found",
            "why": "Speech model, base (multilingual) was not found",
            "error": "Speech model, base (multilingual) not found (searched: PA_SPEECH_MODELS_DIR /home/me/whisper-stuff, app bundle resources \
                      (not running from an app bundle), application data /home/me/.local/share/patools/models)"
        })
    }

    #[test]
    fn a_missing_model_is_explained_without_its_id_or_the_folders_searched() {
        for status in ["no_models_dir", "model_unavailable"] {
            let mut result = not_found();
            result["status"] = json!(status);
            let msg = nothing_stored(&result);
            assert_eq!(
                msg,
                "Speech model, base (multilingual) was not found. Set the Models folder to the folder that contains it, \
                 or leave it empty to use the app's own models folder, then try again."
            );
            assert!(!msg.to_lowercase().contains("whisper") && !msg.contains("/home") && !msg.contains("PA_SPEECH"), "{msg}");
        }
        // An engine that says nothing more than the status still gets a sentence, and still no id.
        let bare = nothing_stored(&json!({"status": "no_models_dir"}));
        assert!(bare.starts_with("The speech model is not available.") && bare.contains("Models folder"), "{bare}");
        // A build without the speech feature has no folder to fix.
        let build = nothing_stored(&json!({"status": "model_unavailable", "reason": "unsupported_build",
            "why": "speech-to-text is not available in this build (this build does not include the speech feature)"}));
        assert_eq!(build, "speech-to-text is not available in this build (this build does not include the speech feature).");
        // Files that are there but unusable: say what is wrong (one line), without the folder.
        let invalid =
            nothing_stored(&json!({"status": "model_unavailable", "reason": "invalid_model", "model_name": "Speech model, base (multilingual)",
            "why": "model: tokenizer.json: <|0.00|> has id 3\n   0: backtrace frame"}));
        assert!(invalid.starts_with("model: tokenizer.json: <|0.00|> has id 3.") && !invalid.contains("backtrace"), "{invalid}");
        assert!(nothing_stored(&json!({"status": "weird"})).contains("weird"));
        assert!(nothing_stored(&json!({})).contains("without a transcript"));
    }

    #[test]
    fn a_status_is_one_line() {
        assert_eq!(first_line("one\ntwo\nthree"), "one");
        assert_eq!(first_line("\n  \n  first real line  \nnext"), "first real line");
        assert_eq!(first_line(""), "");
        let mut app = app();
        set_status(&mut app, "transcription failed: out of memory\nstack backtrace:\n   0: frame\n   1: frame");
        assert_eq!(app.transcript.status, "transcription failed: out of memory");
        assert_eq!(app.ui.status, "transcription failed: out of memory");
    }

    #[test]
    fn the_models_folder_is_sent_only_when_typed() {
        let track = TrackId(7);
        // Empty (the default, however the environment is set) or blank: no `models_dir`, so the
        // engine's search (environment, app bundle, app data) stays in force.
        assert_eq!(TranscriptState::default().models_dir, "");
        for typed in ["", "   ", "\t"] {
            assert_eq!(job_params(track, typed, None), json!({"track": 7}), "{typed:?}");
        }
        assert_eq!(
            job_params(track, "  /Volumes/Models  ", Some("de".into())),
            json!({"track": 7, "models_dir": "/Volumes/Models", "language": "de"})
        );
    }

    #[test]
    fn a_transcript_source_is_shown_by_its_display_name() {
        assert_eq!(made_with("whisper-base"), "Made with Speech model, base (multilingual)");
        assert_eq!(made_with("manual"), "Made with manual");
    }

    #[test]
    fn the_language_comes_from_the_transcription_settings() {
        let mut s = Session::default();
        assert_eq!(language_flag(&s), None);
        s.edit.flags.insert("transcription.enabled".into());
        assert_eq!(language_flag(&s), None);
        s.edit.flags.insert("transcription.language=de".into());
        assert_eq!(language_flag(&s).as_deref(), Some("de"));
    }

    // ---- the worker thread's side of things: a fake job whose messages the test sends ----------

    use crate::Services;

    fn app() -> SoundApp {
        SoundApp::new(soundcraft_engine::demo::demo_engine(), None, Services::default())
    }

    fn first_audio(app: &SoundApp) -> TrackId {
        app.engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio).map(|t| t.id).unwrap()
    }

    /// Puts a running job on `app` whose results the test controls.
    fn fake_job(app: &mut SoundApp, track: TrackId) -> (mpsc::Sender<Msg>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let track_name = app.engine.session().track(track).unwrap().name.clone();
        let stop = Arc::clone(&cancel);
        app.transcript.job = Some(Running { track, track_name, started: 0.0, fraction: 0.0, stage: String::new(), cancel, rx, thread: None });
        (tx, stop)
    }

    fn transcript_of(track: TrackId, n: usize) -> Transcript {
        let words = (0..n)
            .map(|i| TranscriptWord { start: i as i64 * 12_000, end: i as i64 * 12_000 + 9_000, text: format!("word{i}"), speaker: (i % 2) as u8 })
            .collect();
        Transcript { track, words, language: "en".into(), source: "whisper-base".into(), speaker_names: Vec::new() }
    }

    fn done(transcript: Option<Transcript>, result: Value) -> Msg {
        Msg::Done(Ok(TranscribeOutcome { result, transcript }))
    }

    #[test]
    fn progress_is_kept_while_the_job_runs() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (tx, _) = fake_job(&mut app, track);
        tx.send(Msg::Progress(0.25, "listening".into())).unwrap();
        tx.send(Msg::Progress(0.5, "decoding".into())).unwrap();
        tx.send(Msg::Progress(f32::NAN, "ignored number".into())).unwrap();
        step(&mut app, &ctx);
        let job = app.transcript.job.as_ref().expect("still running");
        assert_eq!((job.fraction, job.stage.as_str()), (0.5, "ignored number"), "a NaN fraction keeps the last one");
        assert!(app.transcript.is_running());
    }

    #[test]
    fn a_finished_job_stores_its_transcript_through_the_command() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (tx, _) = fake_job(&mut app, track);
        tx.send(done(Some(transcript_of(track, 22)), json!({"status": "ok"}))).unwrap();
        step(&mut app, &ctx);
        assert!(app.transcript.job.is_none());
        let stored = app.engine.session().transcript(track).expect("stored");
        assert_eq!((stored.words.len(), stored.language.as_str(), stored.source.as_str()), (22, "en", "whisper-base"));
        assert_eq!(stored.words[3].text, "word3");
        assert_eq!(stored.words[3].start, 36_000);
        assert_eq!(stored.words[3].speaker, 1);
        assert_eq!(app.transcript.status, "Transcribed 22 words");
        assert_eq!(app.ui.status, "Transcribed 22 words");
        assert!(app.engine.is_dirty());
    }

    #[test]
    fn one_word_is_not_plural_and_no_words_means_no_speech() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (tx, _) = fake_job(&mut app, track);
        tx.send(done(Some(transcript_of(track, 1)), json!({"status": "ok"}))).unwrap();
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "Transcribed 1 word");
        let (tx, _) = fake_job(&mut app, track);
        tx.send(done(Some(transcript_of(track, 0)), json!({"status": "ok"}))).unwrap();
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "No speech found");
        assert!(app.engine.session().transcript(track).unwrap().words.is_empty(), "an empty transcript is stored: the track was transcribed");
    }

    #[test]
    fn a_job_for_a_track_that_changed_is_discarded() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (tx, _) = fake_job(&mut app, track);
        app.run("track.rename", json!({"track": track.0, "name": "Renamed while working"})).unwrap();
        tx.send(done(Some(transcript_of(track, 5)), json!({"status": "ok"}))).unwrap();
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "Track changed; transcript discarded");
        assert!(app.engine.session().transcript(track).is_none());

        // The same goes for a track that is gone.
        let other = app.engine.session().tracks.iter().find(|t| t.kind == TrackKind::Audio && t.id != track).map(|t| t.id).unwrap();
        let (tx, _) = fake_job(&mut app, other);
        app.run("track.delete", json!({"tracks": [other.0]})).unwrap();
        tx.send(done(Some(transcript_of(other, 5)), json!({"status": "ok"}))).unwrap();
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "Track changed; transcript discarded");
        assert!(app.engine.session().transcript(other).is_none());
    }

    #[test]
    fn a_missing_model_or_an_error_is_shown_and_nothing_is_stored() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        for status in ["no_models_dir", "model_unavailable"] {
            let (tx, _) = fake_job(&mut app, track);
            let mut result = not_found();
            result["status"] = json!(status);
            tx.send(done(None, result)).unwrap();
            step(&mut app, &ctx);
            let shown = &app.transcript.status;
            assert!(shown.contains("Models folder") && shown.contains("was not found"), "{shown}");
            assert!(!shown.to_lowercase().contains("whisper") && !shown.contains("searched"), "{shown}");
        }
        let (tx, _) = fake_job(&mut app, track);
        tx.send(Msg::Done(Err("transcription failed: out of memory".into()))).unwrap();
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "transcription failed: out of memory");
        assert!(app.engine.session().transcript(track).is_none());
        assert!(app.transcript.job.is_none());
    }

    #[test]
    fn a_worker_that_vanishes_is_reported() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (tx, _) = fake_job(&mut app, track);
        drop(tx);
        step(&mut app, &ctx);
        assert!(app.transcript.status.contains("stopped unexpectedly"), "{}", app.transcript.status);
        assert!(app.transcript.job.is_none());
    }

    #[test]
    fn cancel_tells_the_worker_to_stop_and_forgets_the_job() {
        let mut app = app();
        let track = first_audio(&app);
        assert!(!cancel(&mut app), "nothing to cancel");
        let (_tx, stop) = fake_job(&mut app, track);
        assert!(cancel(&mut app));
        assert!(stop.load(Ordering::Relaxed), "the worker's callback will say stop");
        assert!(app.transcript.job.is_none());
        assert_eq!(app.transcript.status, "Transcription stopped");
        assert_eq!(app.ui.status, "Transcription stopped");
    }

    /// A worker thread that stays alive until the returned sender is used (or dropped), standing in
    /// for a recogniser that takes a while to notice it was told to stop.
    fn slow_worker() -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (release, wait) = mpsc::channel::<()>();
        (
            release,
            std::thread::spawn(move || {
                let _ = wait.recv();
            }),
        )
    }

    /// Steps the window until `done`, up to ten seconds.
    fn step_until(app: &mut SoundApp, ctx: &egui::Context, done: impl Fn(&SoundApp) -> bool) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done(app) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
            step(app, ctx);
        }
        assert!(done(app), "timed out; status is `{}`", app.transcript.status);
    }

    /// After Cancel the worker thread may still be running the model. A new job must not start
    /// until it has ended: the window says so, keeps the handle, and starts again once it is done.
    #[test]
    fn after_cancel_no_new_job_starts_until_the_old_thread_has_finished() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (_tx, stop) = fake_job(&mut app, track);
        let (release, thread) = slow_worker();
        if let Some(job) = &mut app.transcript.job {
            job.thread = Some(thread);
        }

        assert!(cancel(&mut app));
        assert!(stop.load(Ordering::Relaxed));
        assert!(app.transcript.job.is_none() && !app.transcript.is_running());
        assert!(app.transcript.is_stopping(), "the old thread is still alive");
        assert_eq!(app.transcript.status, "Stopping the previous transcription…");
        assert_eq!(app.ui.status, "Stopping the previous transcription…");

        // Every way of asking for a job is refused, with the same message, while it winds down.
        start(&mut app, &ctx, track);
        assert!(app.transcript.job.is_none(), "start() refused");
        assert_eq!(app.transcript.status, "Stopping the previous transcription…");
        app.transcript.status.clear();
        crate::menus::invoke_menu(&mut app, "track.transcribe", "Track > Transcribe Track");
        step(&mut app, &ctx);
        assert!(app.transcript.job.is_none() && !app.transcript.is_running(), "the menu entry started nothing");
        assert_eq!(app.transcript.status, "Stopping the previous transcription…");
        step(&mut app, &ctx);
        assert!(app.transcript.is_stopping(), "stepping does not forget a thread that is alive");

        // The old thread ends; the window notices, says so, and a new job can start.
        drop(release);
        step_until(&mut app, &ctx, |app| !app.transcript.is_stopping());
        step(&mut app, &ctx);
        assert!(app.transcript.stopping.is_none(), "the finished thread was joined and let go");
        assert_eq!(app.transcript.status, "Transcription stopped");
        app.transcript.models_dir =
            std::env::temp_dir().join(format!("pa-tools-no-model-stop-{}", std::process::id())).to_string_lossy().into_owned();
        start(&mut app, &ctx, track);
        assert!(app.transcript.job.is_some(), "Transcribe works again");
        step_until(&mut app, &ctx, |app| app.transcript.job.is_none());
    }

    /// The report: when a stopped thread ended, the window said "Transcription stopped" on the main
    /// status line too, wiping the "Recovered ..." (or any other) message said since. Only the
    /// window's own status line changes; the main one changes only while it still shows "Stopping...".
    #[test]
    fn a_stopped_thread_ending_leaves_the_main_status_line_alone() {
        for said_since in [true, false] {
            let (mut app, ctx) = (app(), egui::Context::default());
            let track = first_audio(&app);
            let (_tx, _) = fake_job(&mut app, track);
            let (release, thread) = slow_worker();
            if let Some(job) = &mut app.transcript.job {
                job.thread = Some(thread);
            }
            assert!(cancel(&mut app));
            assert!(app.transcript.is_stopping());
            assert_eq!(app.ui.status, STOPPING);
            let recovered = "Recovered “Song”. Save it to keep the changes.";
            if said_since {
                // What Recover and Open say once they have stopped the job and replaced the session.
                app.ui.status = recovered.to_string();
            }
            drop(release);
            step_until(&mut app, &ctx, |app| !app.transcript.is_stopping());
            step(&mut app, &ctx);
            assert_eq!(app.transcript.status, STOPPED, "the window says it stopped");
            if said_since {
                assert_eq!(app.ui.status, recovered, "the main line keeps what was said after the stop");
            } else {
                assert_eq!(app.ui.status, STOPPED, "the main line still showed `Stopping...`, so it moves on with the window");
            }
        }
    }

    /// A stopped job whose thread has already ended is not "stopping" at all.
    #[test]
    fn cancel_does_not_wait_for_a_thread_that_has_already_ended() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (_tx, _) = fake_job(&mut app, track);
        let thread = std::thread::spawn(|| ());
        while !thread.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if let Some(job) = &mut app.transcript.job {
            job.thread = Some(thread);
        }
        assert!(cancel(&mut app));
        assert!(!app.transcript.is_stopping());
        assert_eq!(app.transcript.status, "Transcription stopped");
        step(&mut app, &ctx);
        assert_eq!(app.transcript.status, "Transcription stopped");
    }

    /// The Transcribe button is greyed out while the old thread winds down, and Cancel is not shown.
    #[test]
    fn the_transcribe_button_is_off_while_the_stopped_thread_is_alive() {
        let mut app = app();
        let track = first_audio(&app);
        app.run("window.transcript", json!({"value": true})).unwrap();
        let (_tx, _) = fake_job(&mut app, track);
        let (release, thread) = slow_worker();
        if let Some(job) = &mut app.transcript.job {
            job.thread = Some(thread);
        }
        assert!(cancel(&mut app));
        let ctx = egui::Context::default();
        let frame = |app: &mut SoundApp| -> Vec<egui::epaint::ClippedShape> {
            let raw =
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            out.textures_delta.clear();
            out.shapes
        };
        // How the text is painted: a greyed-out label differs in colour or opacity from a live one.
        let colour_of = |shapes: &[egui::epaint::ClippedShape], text: &str| {
            shapes.iter().find_map(|c| match &c.shape {
                Shape::Text(t) if t.galley.text() == text => Some((t.fallback_color, t.override_text_color, t.opacity_factor.to_bits())),
                _ => None,
            })
        };
        let mut shapes = Vec::new();
        for _ in 0..4 {
            shapes = frame(&mut app);
        }
        let waiting = colour_of(&shapes, "Transcribe Track").expect("the button is drawn");
        assert!(colour_of(&shapes, "Cancel").is_none(), "there is nothing to cancel");
        assert!(
            shapes.iter().any(|c| matches!(&c.shape, Shape::Text(t) if t.galley.text() == "Stopping the previous transcription…")),
            "the window says why"
        );

        drop(release);
        step_until(&mut app, &ctx, |app| !app.transcript.is_stopping());
        for _ in 0..4 {
            shapes = frame(&mut app);
        }
        let ready = colour_of(&shapes, "Transcribe Track").expect("the button is drawn");
        assert_ne!(waiting, ready, "the button is enabled again (drawn in the normal text colour)");
    }

    #[test]
    fn the_program_looks_for_the_model_unless_a_recogniser_is_injected() {
        let mut app = app();
        assert!(!app.transcript.has_injected_transcriber(), "a fresh window uses the model on disk");
        app.transcript.inject_transcriber(std::sync::Arc::new(soundcraft_pa_speech::FixedTranscriber {
            transcript: soundcraft_pa_speech::Transcript::default(),
            id: "fixed".into(),
        }));
        assert!(app.transcript.has_injected_transcriber());
    }

    #[test]
    fn replacing_the_session_cancels_but_a_failed_open_does_not() {
        for replace in [("session.new", json!({"name": "Fresh"})), ("session.new", json!({"template": "demo"})), ("session.close", json!({}))] {
            let mut app = app();
            let track = first_audio(&app);
            let (_tx, stop) = fake_job(&mut app, track);
            app.run(replace.0, replace.1).unwrap();
            assert!(app.transcript.job.is_none(), "{}", replace.0);
            assert!(stop.load(Ordering::Relaxed), "{}", replace.0);
            assert_eq!(app.transcript.status, "Transcription stopped");
        }
        let mut app = app();
        let track = first_audio(&app);
        let (_tx, stop) = fake_job(&mut app, track);
        assert!(app.run("session.open", json!({"path": "/no/such/folder/missing.scraft"})).is_err());
        assert!(app.transcript.job.is_some() && !stop.load(Ordering::Relaxed), "the session is still the same");
    }

    #[test]
    fn a_second_transcription_does_not_start_while_one_runs() {
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        let (_tx, stop) = fake_job(&mut app, track);
        start(&mut app, &ctx, track);
        assert!(app.transcript.job.is_some() && !stop.load(Ordering::Relaxed), "the running job is left alone");
    }

    #[test]
    fn a_thread_that_cannot_start_leaves_a_message_not_a_panic() {
        // The real thing: start a job against a folder with no model and let it finish on its thread.
        let (mut app, ctx) = (app(), egui::Context::default());
        let track = first_audio(&app);
        app.transcript.models_dir =
            std::env::temp_dir().join(format!("pa-tools-no-model-unit-{}", std::process::id())).to_string_lossy().into_owned();
        start(&mut app, &ctx, track);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.transcript.job.is_some() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
            step(&mut app, &ctx);
        }
        assert!(app.transcript.job.is_none(), "finished");
        assert!(app.transcript.status.contains("Models folder"), "{}", app.transcript.status);
        assert!(app.engine.session().transcript(track).is_none());
    }

    #[test]
    fn a_track_that_is_gone_cannot_be_transcribed() {
        let (mut app, ctx) = (app(), egui::Context::default());
        start(&mut app, &ctx, TrackId(987_654));
        assert!(app.transcript.job.is_none());
        assert!(app.transcript.status.contains("no longer exists"), "{}", app.transcript.status);
    }

    #[test]
    fn naming_a_speaker_goes_through_the_command() {
        let mut app = app();
        let track = first_audio(&app);
        app.run("track.set_transcript", json!({"track": track.0, "words": [{"start": 0, "end": 10, "text": "hi", "speaker": 1}]})).unwrap();
        assert!(rename_speaker(&mut app, track, 1, "  Dana "));
        assert_eq!(app.engine.session().transcript(track).unwrap().speaker_label(1), "Dana");
        assert_eq!(app.engine.undo_label(), Some("Rename Speaker"));
        assert!(rename_speaker(&mut app, track, 1, ""), "an empty name resets");
        assert_eq!(app.engine.session().transcript(track).unwrap().speaker_label(1), "Speaker 1");
        assert!(!rename_speaker(&mut app, TrackId(987_654), 1, "x"), "an unknown track is refused");
        assert!(!app.ui.status.is_empty(), "and the status line says why");
    }

    #[test]
    fn the_cancel_button_stops_the_job() {
        let mut app = app();
        let track = first_audio(&app);
        app.run("window.transcript", json!({"value": true})).unwrap();
        let (_tx, stop) = fake_job(&mut app, track);
        let ctx = egui::Context::default();
        let frame = |app: &mut SoundApp, events: Vec<egui::Event>| -> Vec<egui::epaint::ClippedShape> {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            out.textures_delta.clear();
            out.shapes
        };
        let mut shapes = Vec::new();
        for _ in 0..5 {
            shapes = frame(&mut app, vec![]);
        }
        let find = |shapes: &[egui::epaint::ClippedShape], text: &str| {
            shapes.iter().find_map(|c| match &c.shape {
                Shape::Text(t) if t.galley.text() == text => Some(t.galley.rect.translate(t.pos.to_vec2()).center()),
                _ => None,
            })
        };
        let at = find(&shapes, "Cancel").expect("a Cancel button while it runs");
        assert!(find(&shapes, "Transcribe Track").is_some(), "the Transcribe Track button is still there, greyed out");
        let click = |pressed| {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
            ]
        };
        frame(&mut app, click(true));
        frame(&mut app, click(false));
        for _ in 0..3 {
            shapes = frame(&mut app, vec![]);
        }
        assert!(app.transcript.job.is_none());
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(app.transcript.status, "Transcription stopped");
        assert!(find(&shapes, "Cancel").is_none(), "the button goes away");
    }

    /// Renders the window in several states to PNGs for a person to look at. Not a check: it asserts
    /// nothing. `TRANSCRIPT_SHOT_DIR=/some/dir cargo test -p soundcraft-ui-egui --lib render_states -- --ignored`.
    #[test]
    #[ignore = "writes screenshots for review; needs TRANSCRIPT_SHOT_DIR"]
    fn render_states_for_review() {
        let Some(dir) = std::env::var_os("TRANSCRIPT_SHOT_DIR").map(std::path::PathBuf::from) else { return };
        let words: Vec<Value> = {
            let text = "Welcome back to the show. Today we are talking about how a small studio finishes a record. \
                        Thanks for having me. I have been mixing from this room for about eleven years now. \
                        Eleven years! What is the first thing you do when a session arrives?";
            let mut at = 24_000i64;
            let mut speaker = 0u8;
            text.split_whitespace()
                .map(|w| {
                    let start = at;
                    at += 11_000 + w.len() as i64 * 1_500;
                    if w == "record." || w == "now." {
                        speaker = (speaker + 1) % 2;
                        at += 40_000;
                    }
                    json!({"start": start, "end": at - 4_000, "text": w, "speaker": speaker})
                })
                .collect()
        };
        let shoot = |name: &str, light: bool, setup: &dyn Fn(&mut SoundApp)| {
            let mut app = app();
            let track = first_audio(&app);
            app.run("track.set_transcript", json!({"track": track.0, "words": words.clone(), "language": "en", "source": "whisper-base"})).unwrap();
            app.run("track.rename_speaker", json!({"track": track.0, "speaker": 1, "name": "Guest"})).unwrap();
            app.run("window.transcript", json!({"value": true})).unwrap();
            app.run("ui.theme", json!({"mode": if light { "light" } else { "dark" }})).unwrap();
            let mut harness = egui_kittest::Harness::builder().with_size(vec2(720.0, 600.0)).with_pixels_per_point(2.0).wgpu().build_ui_state(
                |ui, app: &mut SoundApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            for _ in 0..4 {
                harness.step();
            }
            // Only now: showing a track for the first time clears a half-typed name.
            setup(harness.state_mut());
            for _ in 0..4 {
                harness.step();
            }
            let image = harness.render().expect("render");
            image.save(dir.join(name)).expect("save");
        };
        shoot("running.png", false, &|app| {
            let track = first_audio(app);
            let (tx, _) = fake_job(app, track);
            std::mem::forget(tx);
            if let Some(job) = &mut app.transcript.job {
                (job.fraction, job.stage) = (0.42, "decoding speech".into());
            }
            app.transcript.status = "Transcribing Kick...".into();
        });
        shoot("search.png", false, &|app| {
            app.transcript.query = "the".into();
            app.transcript.status = "Transcribed 38 words".into();
        });
        shoot("rename.png", false, &|app| {
            app.transcript.editing = Some(SpeakerEdit { row: 1, speaker: 0, text: "Speaker 0".into(), focus: true, fresh: true, seen: false });
        });
        shoot("model-missing.png", false, &|app| {
            app.transcript.status = nothing_stored(&not_found());
        });
        shoot("empty.png", false, &|app| {
            let track = first_audio(app);
            app.engine.session_mut().transcripts.retain(|t| t.track != track);
        });
        shoot("light.png", true, &|app| {
            app.transcript.query = "years".into();
        });
    }

    #[test]
    fn elapsed_time_reads_as_minutes_and_seconds() {
        assert_eq!(elapsed_text(0.0), "0:00");
        assert_eq!(elapsed_text(75.9), "1:15");
        assert_eq!(elapsed_text(-3.0), "0:00");
        assert_eq!(elapsed_text(f64::NAN), "0:00");
    }
}

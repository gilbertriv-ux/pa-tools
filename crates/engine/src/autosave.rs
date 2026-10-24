//! Immutable crash-recovery snapshots written in the background.
//!
//! Every committed snapshot has an exclusive directory and a completion marker written last.
//! Closing or dismissing recovery writes a marker; original snapshots and audio are retained.
//! Recovery never rewrites a take: stale headers are repaired in exclusive sibling copies.

use crate::io::{Recording, open_session_linked, sanitize_name};
use crate::{Engine, EngineError, Result};
use soundcraft_audio_io::{BitDepth, EncodeOptions, FileFormat};
use soundcraft_model::{Session, SourceAudio, SourceId};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

const SESSION: &str = "session.scraft";
const ORIGIN: &str = "origin";
const LOCK: &str = "lock";
const SNAPSHOTS: &str = "Snapshots";
const COMPLETE: &str = "complete";
const CLOSED: &str = "closed";
/// The plan of a recording still running when the folder was written.
const RECORDING: &str = "recording.json";
/// While changes keep coming, the newest is written at most this often.
const INTERVAL: Duration = Duration::from_secs(1);
const STOPPED: &str = "the autosave thread stopped";

/// Writes autosaves on a thread of its own, so the UI never waits on the disk.
pub struct Autosaver {
    jobs: Option<Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    failure: Arc<Mutex<Option<String>>>,
}

enum Job {
    Write(Arc<Session>, Option<String>, Option<Recording>),
    /// Done with the session's folder: offer it next launch, or mark it closed while retaining it.
    Close(bool),
    Flush(Sender<std::result::Result<(), String>>),
}

impl Autosaver {
    /// A writer for autosave folders under `root`.
    pub fn new(root: PathBuf) -> Autosaver {
        let (tx, rx) = channel();
        let failure = Arc::new(Mutex::new(None));
        let writer = Writer { root, failure: Arc::clone(&failure), folder: None, pending: None, last: None, written: Ok(()) };
        let thread = std::thread::Builder::new().name("soundcraft-autosave".into()).spawn(move || writer.run(&rx));
        if let Err(e) = &thread {
            report(&failure, format!("{STOPPED}: {e}"));
        }
        Autosaver { jobs: Some(tx), thread: thread.ok(), failure }
    }

    /// Autosave `session`, opened from the session file `origin`, soon, with the plan of the
    /// recording running in it.
    pub fn write(&self, session: Arc<Session>, origin: Option<String>, recording: Option<Recording>) {
        self.send(Job::Write(session, origin, recording));
    }

    /// Done with the open session's folder: offer recovery next launch, or retain it as closed.
    pub fn close(&self, keep: bool) {
        self.send(Job::Close(keep));
    }

    /// Wait until everything handed over so far is done. A failure returned here isn't reported
    /// again by `failure`.
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = channel();
        self.send(Job::Flush(tx));
        let done = rx.recv().unwrap_or_else(|_| Err(STOPPED.into()));
        if done.is_err() {
            self.failure();
        }
        done.map_err(EngineError::Io)
    }

    /// The newest failure since the last call.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    fn send(&self, job: Job) {
        if self.jobs.as_ref().is_none_or(|tx| tx.send(job).is_err()) {
            report(&self.failure, STOPPED.into());
        }
    }
}

impl Drop for Autosaver {
    /// What was handed over is written, and an open folder stays for the next launch.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The writer thread.
struct Writer {
    root: PathBuf,
    failure: Arc<Mutex<Option<String>>>,
    /// The open session's folder.
    folder: Option<Autosave>,
    /// The newest session not written yet.
    pending: Option<(Arc<Session>, Option<String>, Option<Recording>)>,
    last: Option<Instant>,
    /// How the newest write into the open folder went, for `Flush`.
    written: std::result::Result<(), String>,
}

impl Writer {
    fn run(mut self, jobs: &Receiver<Job>) {
        loop {
            let wait = match (&self.pending, self.last) {
                (None, _) => Duration::MAX,
                (Some(_), last) => last.map_or(Duration::ZERO, |t| INTERVAL.saturating_sub(t.elapsed())),
            };
            match jobs.recv_timeout(wait) {
                Ok(Job::Write(session, origin, recording)) => self.pending = Some((session, origin, recording)),
                Ok(Job::Flush(reply)) => {
                    self.write_pending();
                    let _ = reply.send(self.written.clone());
                }
                Ok(Job::Close(keep)) => {
                    if keep {
                        self.write_pending();
                    }
                    self.pending = None;
                    self.written = Ok(());
                    if let Some(a) = self.folder.take()
                        && !keep
                    {
                        a.discard();
                    }
                }
                Err(RecvTimeoutError::Timeout) => self.write_pending(),
                Err(RecvTimeoutError::Disconnected) => {
                    self.write_pending();
                    return;
                }
            }
        }
    }

    fn write_pending(&mut self) {
        let Some((session, origin, recording)) = self.pending.take() else { return };
        self.last = Some(Instant::now());
        let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            if self.folder.is_none() {
                self.folder = Some(Autosave::create(&self.root, &session.name)?);
            }
            self.folder.as_mut().map_or(Ok(()), |a| a.write(&session, origin.as_deref(), recording.as_ref()))
        }));
        self.written = match written {
            Ok(written) => written.map_err(|e| e.to_string()),
            Err(_) => Err("internal error (see the log)".into()),
        };
        if let Err(e) = &self.written {
            report(&self.failure, e.clone());
        }
    }
}

fn report(failure: &Mutex<Option<String>>, e: String) {
    log::warn!("autosave failed: {e}");
    *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
}

/// An open session's autosave folder, locked while the session is open so a later launch can tell
/// it from one a crash left behind.
struct Autosave {
    dir: PathBuf,
    lock: Option<std::fs::File>,
    copies: BTreeMap<SourceId, (Weak<SourceAudio>, String)>,
    next: u64,
    /// Exclusive snapshot generation, independent of the audio-copy sequence.
    snapshot: u64,
}

impl Autosave {
    /// A new folder under `root` for the session called `name`.
    fn create(root: &Path, name: &str) -> Result<Autosave> {
        static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::path::absolute(root).map_err(|err| io_error(root, err))?;
        std::fs::create_dir_all(&root).map_err(|err| io_error(&root, err))?;
        for _ in 0..1_000 {
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
            let serial = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = root.join(format!("{} {stamp}-{}-{serial}", sanitize_name(name), std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    let lock = std::fs::File::create_new(dir.join(LOCK)).map_err(|err| io_error(&dir, err))?;
                    lock.try_lock().map_err(|err| io_error(&dir, err.into()))?;
                    return Ok(Autosave { dir, lock: Some(lock), copies: BTreeMap::new(), next: 1, snapshot: 1 });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(io_error(&dir, err)),
            }
        }
        Err(EngineError::Io("cannot reserve a new autosave folder".into()))
    }

    /// Write `session`, opened from the session file `origin`, and the plan of the `recording`
    /// running in it. Audio saved with it is referenced where it is; audio that exists only in
    /// memory is copied, once per recording or render.
    fn write(&mut self, session: &Session, origin: Option<&str>, recording: Option<&Recording>) -> Result<()> {
        let base = origin.and_then(|p| Path::new(p).parent()).and_then(|p| std::path::absolute(p).ok());
        let audio_dir = self.dir.join("Audio Files");
        let mut s = session.clone();
        let mut copies = BTreeMap::new();
        for src in &mut s.sources {
            let file = if Path::new(&src.path).is_absolute() { Some(PathBuf::from(&src.path)) } else { base.as_ref().map(|b| b.join(&src.path)) };
            let audio = s.pool.get(src.id);
            if let Some(f) = &file
                && (audio.is_none() || !src.unsaved && f.is_file())
            {
                src.path = f.to_string_lossy().into_owned();
                continue;
            }
            let Some(audio) = audio else { continue };
            let name = match self.copies.get(&src.id) {
                Some((copy, name)) if copy.upgrade().is_some_and(|a| Arc::ptr_eq(&a, audio)) => name.clone(),
                _ => {
                    let name = format!("{} {}.wav", sanitize_name(&src.name), self.next);
                    self.next = self.next.checked_add(1).ok_or_else(|| EngineError::Io("autosave audio generation overflow".into()))?;
                    let opts = EncodeOptions { format: FileFormat::Wav, bit_depth: BitDepth::Float32, dither: false, bwf: None };
                    std::fs::create_dir_all(&audio_dir).map_err(|err| io_error(&audio_dir, err))?;
                    crate::io::write_audio_new(&audio_dir.join(&name), audio, &opts)?;
                    name
                }
            };
            src.path = audio_dir.join(&name).to_string_lossy().into_owned();
            src.unsaved = true;
            copies.insert(src.id, (Arc::downgrade(audio), name));
        }
        let text = s.to_json().map_err(|err| EngineError::Io(err.to_string()))?;
        let snapshots = self.dir.join(SNAPSHOTS);
        std::fs::create_dir_all(&snapshots).map_err(|err| io_error(&snapshots, err))?;
        let snapshot = loop {
            let generation = self.snapshot;
            self.snapshot = generation.checked_add(1).ok_or_else(|| EngineError::Io("autosave snapshot generation overflow".into()))?;
            let path = snapshots.join(format!("{generation:020}.snapshot"));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(io_error(&path, err)),
            }
        };
        write_new_synced(&snapshot.join(ORIGIN), origin.unwrap_or_default().as_bytes()).map_err(|err| io_error(&snapshot, err))?;
        write_new_synced(&snapshot.join(SESSION), text.as_bytes()).map_err(|err| io_error(&snapshot, err))?;
        if let Some(r) = recording {
            let plan = serde_json::to_string(r).map_err(|err| EngineError::Io(err.to_string()))?;
            write_new_synced(&snapshot.join(RECORDING), plan.as_bytes()).map_err(|err| io_error(&snapshot, err))?;
        }
        // Make media-directory entries durable before committing the snapshot that names them.
        if audio_dir.is_dir() {
            sync_directory(&audio_dir).map_err(|err| io_error(&audio_dir, err))?;
        }
        sync_directory(&self.dir).map_err(|err| io_error(&self.dir, err))?;
        // A partial directory is retained but never selected by recovery.
        write_new_synced(&snapshot.join(COMPLETE), b"complete-v1\n").map_err(|err| io_error(&snapshot, err))?;
        sync_directory(&snapshot)
            .and_then(|()| sync_directory(&snapshots))
            .and_then(|()| sync_directory(&self.dir))
            .map_err(|err| io_error(&snapshot, err))?;
        self.copies = copies;
        Ok(())
    }

    /// Retain the folder and mark it closed, so it is no longer offered for recovery.
    fn discard(mut self) {
        if let Err(err) = mark_closed(&self.dir) {
            log::warn!("cannot mark autosave closed: {err}");
        }
        drop(self.lock.take());
    }
}

/// Unsaved work left behind in an autosave folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Leftover {
    pub dir: PathBuf,
    /// The session's name.
    pub name: String,
    /// The session file it was autosaved from, if it had one.
    pub origin: Option<String>,
    pub saved: SystemTime,
    /// A recording was still running in it.
    pub recording: bool,
}

/// Leftover autosaves under `root`, newest first. Closed, partial and outdated snapshots are
/// ignored, never deleted. The root lock still protects all of its immutable generations.
pub fn leftovers(root: &Path) -> Vec<Leftover> {
    let mut out = Vec::new();
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten().map(|d| d.path()) {
        let Ok(lock) = std::fs::OpenOptions::new().write(true).open(dir.join(LOCK)) else { continue };
        if lock.try_lock().is_err() || is_closed(&dir) {
            continue;
        }
        let Some(snapshot) = latest_snapshot(&dir) else { continue };
        if is_closed(&snapshot) {
            continue;
        }
        let origin = std::fs::read_to_string(snapshot.join(ORIGIN)).ok().filter(|o| !o.is_empty());
        let origin_saved = origin.as_ref().and_then(|o| std::fs::metadata(o).and_then(|m| m.modified()).ok());
        let saved = std::fs::metadata(snapshot.join(SESSION)).and_then(|m| m.modified());
        let recording = snapshot.join(RECORDING).is_file();
        if let Ok(saved) = saved
            && (recording || origin_saved.is_none_or(|o| saved > o))
        {
            let text = std::fs::read_to_string(snapshot.join(SESSION)).unwrap_or_default();
            let named = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.get("name")?.as_str().map(str::to_string));
            let name = named.unwrap_or_else(|| dir.file_name().unwrap_or_default().to_string_lossy().into_owned());
            out.push(Leftover { dir: snapshot, name, origin, saved, recording });
        }
        drop(lock);
    }
    out.sort_by_key(|l| std::cmp::Reverse(l.saved));
    out
}

/// The latest committed generation, with the old root layout retained as a read fallback.
fn latest_snapshot(dir: &Path) -> Option<PathBuf> {
    let latest = std::fs::read_dir(dir.join(SNAPSHOTS))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            let name = entry.file_name();
            let generation = name.to_str()?.strip_suffix(".snapshot")?.parse::<u64>().ok()?;
            let complete = path.join(COMPLETE);
            if !std::fs::symlink_metadata(&complete).ok()?.file_type().is_file()
                || std::fs::metadata(&complete).ok()?.len() != 12
                || std::fs::read(&complete).ok()?.as_slice() != b"complete-v1\n"
                || !path.join(SESSION).is_file()
            {
                return None;
            }
            Some((generation, path))
        })
        .max_by_key(|(generation, _)| *generation);
    latest.map(|(_, path)| path).or_else(|| dir.join(SESSION).is_file().then(|| dir.to_path_buf()))
}

/// `Leftover.dir` is the directory containing its session, either a snapshot or a legacy root.
fn autosave_root(dir: &Path) -> &Path {
    match dir.parent() {
        Some(parent) if parent.file_name().is_some_and(|n| n == SNAPSHOTS) && dir.extension().is_some_and(|n| n == "snapshot") => {
            parent.parent().unwrap_or(dir)
        }
        _ => dir,
    }
}

/// Open `left` as an unsaved version of the session it came from, so Save writes it back there,
/// with the takes of a recording that never finished placed as Stop would have. Returns the
/// media that couldn't be loaded.
pub fn recover(e: &mut Engine, left: &Leftover) -> Result<Vec<String>> {
    let mut r = Engine::default();
    let session = left.dir.join(SESSION);
    let links = left.origin.as_deref().map_or(session.as_path(), Path::new);
    let mut missing = open_session_linked(&mut r, &session.to_string_lossy(), links)?;
    let mut s = r.session().clone();
    let unfinished = unfinished_takes(left, &s, &mut missing);
    if let Some(home) = left.origin.as_deref().and_then(|o| Path::new(o).parent()) {
        for src in s.sources.iter_mut().filter(|src| !src.unsaved) {
            if let Ok(rel) = Path::new(&src.path).strip_prefix(home) {
                src.path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            }
        }
    }
    e.replace_session(Session::default());
    *e.session_mut() = s;
    e.path = left.origin.clone();
    if let Some((plan, takes)) = unfinished {
        crate::io::place_recording(e, &plan, takes);
    }
    Ok(missing)
}

/// The takes of the recording `left` holds the plan of, repaired, minus those already in `s`.
/// A take the plan names that isn't a take file SoundCraft recorded, in a folder it records into,
/// is left untouched and added to `skipped`.
fn unfinished_takes(left: &Leftover, s: &Session, skipped: &mut Vec<String>) -> Option<(Recording, Vec<(soundcraft_model::TrackId, PathBuf)>)> {
    let plan: Recording = serde_json::from_str(&std::fs::read_to_string(left.dir.join(RECORDING)).ok()?).ok()?;
    let known: Vec<PathBuf> =
        s.sources.iter().filter(|src| s.pool.get(src.id).is_some()).filter_map(|src| std::fs::canonicalize(&src.path).ok()).collect();
    let folders = take_folders(left);
    let mut takes = Vec::new();
    for t in &plan.takes {
        if std::fs::canonicalize(&t.path).is_ok_and(|p| known.contains(&p)) {
            continue;
        }
        match repair_take(&t.path, &folders, t.channels, plan.rate) {
            Ok((frames, recovered)) if frames > 0 => {
                if !std::fs::canonicalize(&recovered).is_ok_and(|p| known.contains(&p)) {
                    takes.push((t.track, recovered));
                }
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!("recovery skipped the take {}: {err}", t.path.display());
                skipped.push(format!("{}: {err}", t.path.display()));
            }
        }
    }
    Some((plan, takes))
}

/// The folders a recording in `left`'s session writes its takes into: the `Audio Files` next to
/// the session file, or, never saved, the one in its folder under the autosave root.
fn take_folders(left: &Leftover) -> Vec<PathBuf> {
    let saved = left.origin.as_deref().and_then(|o| Path::new(o).parent()).map(|d| d.join("Audio Files"));
    let unsaved = autosave_root(&left.dir).parent().map(|root| root.join(sanitize_name(&left.name)).join("Audio Files"));
    saved.into_iter().chain(unsaved).filter_map(|d| std::fs::canonicalize(d).ok()).collect()
}

/// Validate an unfinished take read-only. When its sizes are stale, create a corrected sibling
/// copy and return that path; the original header, payload and trailing bytes stay untouched.
fn repair_take(path: &Path, folders: &[PathBuf], channels: usize, rate: u32) -> std::io::Result<(u64, PathBuf)> {
    use std::io::{Read, Seek, SeekFrom};
    let refuse = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string());
    crate::file_access::check_read(path).map_err(|why| refuse(&why))?;
    if channels == 0 || channels > usize::from(u16::MAX) || rate == 0 || !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(refuse("not a regular take with a valid channel count and rate"));
    }
    let folder = path.parent().and_then(|d| std::fs::canonicalize(d).ok());
    if !folder.is_some_and(|d| folders.contains(&d)) {
        return Err(refuse(&format!("not in a folder {} records into", soundcraft_brand::APP_NAME)));
    }
    let header = |frames| soundcraft_audio_io::wav_float_header(channels, rate, frames).map_err(std::io::Error::other);
    let expected = header(0)?;
    let mut source = std::fs::File::open(path)?;
    let mut head = vec![0u8; expected.len()];
    if source.read_exact(&mut head).is_err() || !is_take_header(&head, &expected) {
        return Err(refuse(&format!("not a take {} recorded", soundcraft_brand::APP_NAME)));
    }
    let audio = source.metadata()?.len().saturating_sub(expected.len() as u64);
    let frame_bytes = channels as u64 * 4;
    let frames = audio / frame_bytes;
    let repaired_header = header(frames)?;
    if frames == 0 || head == repaired_header {
        return Ok((frames, path.to_path_buf()));
    }
    let payload = frames * frame_bytes;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let parent = path.parent().ok_or_else(|| refuse("take has no parent folder"))?;
    for generation in 0..1_000u64 {
        let suffix = if generation == 0 { String::new() } else { format!("-{generation}") };
        let copy = parent.join(format!("{stem}.recovered-{frames}{suffix}.wav"));
        crate::file_access::check_write(&copy).map_err(|why| refuse(&why))?;
        match std::fs::File::create_new(&copy) {
            Ok(mut destination) => {
                destination.write_all(&repaired_header)?;
                source.seek(SeekFrom::Start(expected.len() as u64))?;
                let copied = std::io::copy(&mut Read::take(&mut source, payload), &mut destination)?;
                if copied != payload {
                    return Err(refuse("take changed or was truncated during recovery"));
                }
                destination.sync_all()?;
                sync_directory(parent)?;
                return Ok((frames, copy));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if matches_recovery(&mut source, &copy, &repaired_header, payload)? {
                    return Ok((frames, copy));
                }
            }
            Err(err) => return Err(err),
        }
    }
    Err(refuse("cannot reserve a new recovered take file"))
}

/// Reuse a prior recovery only after a bounded, byte-for-byte comparison with its source.
fn matches_recovery(source: &mut std::fs::File, candidate: &Path, header: &[u8], payload: u64) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    if !std::fs::symlink_metadata(candidate).is_ok_and(|m| m.file_type().is_file() && m.len() == header.len() as u64 + payload) {
        return Ok(false);
    }
    let Ok(mut other) = std::fs::File::open(candidate) else { return Ok(false) };
    let mut head = vec![0u8; header.len()];
    if other.read_exact(&mut head).is_err() || head != header {
        return Ok(false);
    }
    source.seek(SeekFrom::Start(header.len() as u64))?;
    let (mut left, mut right) = ([0u8; 8192], [0u8; 8192]);
    let mut remaining = payload;
    while remaining > 0 {
        let n = usize::try_from(remaining.min(left.len() as u64)).map_err(std::io::Error::other)?;
        let (Some(a), Some(b)) = (left.get_mut(..n), right.get_mut(..n)) else { return Ok(false) };
        source.read_exact(a)?;
        if other.read_exact(b).is_err() || a != b {
            return Ok(false);
        }
        remaining -= n as u64;
    }
    Ok(true)
}

/// `head` is a take header like `expected` (the recorder's header for no frames yet): RIFF or
/// RF64 WAVE, the same format chunk at the same place, and the data chunk right after it all.
/// Only the sizes and frame counts may differ.
fn is_take_header(head: &[u8], expected: &[u8]) -> bool {
    let magic = matches!(head.get(..4), Some(b"RIFF" | b"RF64")) && head.get(8..12) == Some(b"WAVE");
    let fmt = expected.windows(4).position(|w| w == b"fmt ");
    let fmt_len = fmt.and_then(|at| expected.get(at + 4..at + 8)).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    let same_fmt = match (fmt, fmt_len) {
        (Some(at), Some(n)) => {
            let end = at.saturating_add(8).saturating_add(n as usize);
            expected.get(at..end).is_some_and(|chunk| head.get(at..end) == Some(chunk))
        }
        _ => false,
    };
    let data = expected.len().checked_sub(8).and_then(|at| head.get(at..at + 4)) == Some(b"data");
    magic && same_fmt && data
}

/// Dismiss a leftover without removing its session, media or recording plan.
pub fn discard(left: &Leftover) {
    if let Err(err) = mark_closed(&left.dir) {
        log::warn!("cannot dismiss retained autosave: {err}");
    }
}

fn is_closed(dir: &Path) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|entry| {
        let name = entry.file_name();
        let marker = name.to_str().is_some_and(|name| name == CLOSED || name.strip_prefix("closed-").is_some_and(|n| n.parse::<u64>().is_ok()));
        marker
            && entry.file_type().is_ok_and(|t| t.is_file())
            && entry.metadata().is_ok_and(|m| m.len() == 10)
            && std::fs::read(entry.path()).is_ok_and(|bytes| bytes == b"closed-v1\n")
    })
}

fn mark_closed(dir: &Path) -> std::io::Result<()> {
    if is_closed(dir) {
        return Ok(());
    }
    for generation in 0..1_000u64 {
        let name = if generation == 0 { CLOSED.to_string() } else { format!("closed-{generation}") };
        match write_new_synced(&dir.join(name), b"closed-v1\n") {
            Ok(()) => return sync_directory(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("cannot reserve a new autosave closed marker"))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create_new(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn io_error(path: &Path, err: std::io::Error) -> EngineError {
    EngineError::Io(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::open_session;
    use serde_json::json;
    use soundcraft_audio_io::AudioBuffer;
    use soundcraft_model::{ChannelFormat, Clip, TrackKind};

    fn folder(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..1_000 {
            let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("pa-retained-{name}-{}-{stamp}-{serial}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => return dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot reserve retained test directory: {e}"),
            }
        }
        panic!("cannot reserve a unique retained test directory")
    }

    fn record(e: &mut Engine, level: f32) {
        let before = e.session_arc();
        let s = e.session_mut();
        let t = match s.tracks.first() {
            Some(t) => t.id,
            None => s.add_track(TrackKind::Audio, ChannelFormat::Mono, None),
        };
        let src =
            crate::io::add_source(s, "Audio 1_01", AudioBuffer { sample_rate: 48_000, channels: vec![vec![level; 4_800]] }, None, FileFormat::Wav);
        let id = s.new_clip_id();
        crate::edit::place_clip(s, t, Clip::audio(id, "Audio 1_01", src, 0, 0, 4_800));
        e.push_undo("Record", before);
    }

    fn levels(e: &Engine) -> Vec<f32> {
        let s = e.session();
        s.sources.iter().map(|src| s.pool.get(src.id).map_or(f32::NAN, |a| a.sample(0, 0, false).unwrap_or(f32::NAN))).collect()
    }

    #[test]
    fn immutable_snapshots_keep_prior_bytes_and_ignore_partial_generations() {
        let root = folder("snapshot-history");
        let mut a = Autosave::create(&root, "History").unwrap();
        let dir = a.dir.clone();
        let mut session = Session::default();
        session.name = "First".into();
        a.write(&session, None, None).unwrap();
        let first = latest_snapshot(&dir).unwrap();
        let bytes = std::fs::read(first.join(SESSION)).unwrap();
        session.name = "Second".into();
        a.write(&session, None, None).unwrap();
        let second = latest_snapshot(&dir).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first.join(SESSION)).unwrap(), bytes);
        let partial = dir.join(SNAPSHOTS).join("9999999999999999999.snapshot");
        std::fs::create_dir(&partial).unwrap();
        write_new_synced(&partial.join(SESSION), b"interrupted write").unwrap();
        write_new_synced(&partial.join(COMPLETE), b"incomplete").unwrap();
        assert_eq!(latest_snapshot(&dir).unwrap(), second);
        assert!(leftovers(&root).is_empty(), "an open root lock protects every generation");
        drop(a);
        let found = leftovers(&root);
        assert_eq!((found.len(), found[0].dir.clone()), (1, second.clone()));
        assert_eq!(found[0].name, "Second");
        discard(&found[0]);
        assert!(leftovers(&root).is_empty());
        assert!(first.join(SESSION).is_file() && second.join(SESSION).is_file() && partial.join(SESSION).is_file());
    }

    #[test]
    fn incomplete_closed_marker_does_not_hide_recovery() {
        let root = folder("partial-close");
        let mut a = Autosave::create(&root, "Keep").unwrap();
        a.write(&Session::default(), None, None).unwrap();
        let dir = a.dir.clone();
        write_new_synced(&dir.join(CLOSED), b"partial").unwrap();
        drop(a);
        assert_eq!(leftovers(&root).len(), 1);
        mark_closed(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(CLOSED)).unwrap(), b"partial");
        assert!(dir.join("closed-1").is_file());
        assert!(leftovers(&root).is_empty());
    }

    #[test]
    fn stale_take_recovery_keeps_original_and_reuses_only_verified_copy() {
        let dir = folder("copied-recovery");
        let take = dir.join("take.wav");
        let original = [soundcraft_audio_io::wav_float_header(1, 48_000, 1).unwrap(), vec![0u8; 64]].concat();
        write_new_synced(&take, &original).unwrap();
        let allowed = vec![std::fs::canonicalize(&dir).unwrap()];
        let (frames, recovered) = repair_take(&take, &allowed, 1, 48_000).unwrap();
        assert_eq!(frames, 16);
        assert_ne!(recovered, take);
        assert_eq!(std::fs::read(&take).unwrap(), original);
        assert_eq!(repair_take(&take, &allowed, 1, 48_000).unwrap(), (frames, recovered.clone()));
        let bytes = std::fs::read(&recovered).unwrap();
        let (info, _) = soundcraft_audio_io::decode(&bytes, Some("wav")).unwrap();
        assert_eq!(info.frames, 16);
        assert_eq!(std::fs::read(&take).unwrap(), original);
    }

    #[test]
    fn legacy_root_layout_still_scans_without_rewriting_it() {
        let root = folder("legacy-recovery");
        let dir = root.join("Legacy");
        std::fs::create_dir(&dir).unwrap();
        write_new_synced(&dir.join(LOCK), b"").unwrap();
        let session = Session::default().to_json().unwrap();
        write_new_synced(&dir.join(SESSION), session.as_bytes()).unwrap();
        write_new_synced(&dir.join(ORIGIN), b"").unwrap();
        let found = leftovers(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir, dir);
        assert_eq!(std::fs::read_to_string(dir.join(SESSION)).unwrap(), session);
    }

    #[test]
    fn saved_audio_is_referenced_and_takes_are_copied_once() {
        let dir = folder("autosave-copies");
        let path = dir.join("Song/Song.scraft");
        let mut e = Engine::default();
        record(&mut e, 0.5);
        crate::io::save_session(&mut e, &path.to_string_lossy()).unwrap();
        record(&mut e, -0.25);
        let mut a = Autosave::create(&dir.join("Autosave"), "Song").unwrap();
        a.write(e.session(), e.path.as_deref(), None).unwrap();
        a.write(e.session(), e.path.as_deref(), None).unwrap();
        assert_eq!(std::fs::read_dir(a.dir.join("Audio Files")).unwrap().count(), 1);
        drop(a);
        let left = leftovers(&dir.join("Autosave"));
        let mut r = Engine::default();
        assert_eq!(recover(&mut r, &left[0]).unwrap(), Vec::<String>::new());
        let paths: Vec<_> = r.session().sources.iter().map(|s| (s.path.starts_with("Audio Files/"), s.unsaved)).collect();
        assert_eq!(paths, [(true, false), (false, true)]);
        assert_eq!(levels(&r), [0.5, -0.25]);
    }

    #[test]
    fn a_take_recorded_after_undo_gets_its_own_copy() {
        let dir = folder("autosave-undo");
        let mut e = Engine::default();
        record(&mut e, 0.5);
        let mut a = Autosave::create(&dir, "Untitled").unwrap();
        a.write(e.session(), None, None).unwrap();
        e.execute("edit.undo", &json!({})).unwrap();
        record(&mut e, -0.25);
        a.write(e.session(), None, None).unwrap();
        drop(a);
        let mut r = Engine::default();
        recover(&mut r, &leftovers(&dir)[0]).unwrap();
        assert_eq!(levels(&r), [-0.25]);
    }

    #[test]
    fn the_writer_saves_the_newest_change_and_retains_the_closed_folder() {
        let root = folder("autosave-writer");
        let w = Autosaver::new(root.clone());
        let mut e = Engine::default();
        for _ in 0..5 {
            record(&mut e, 0.5);
            w.write(e.session_arc(), None, None);
        }
        w.flush().unwrap();
        let file =
            std::fs::read_dir(&root).unwrap().flatten().filter_map(|d| latest_snapshot(&d.path())).map(|d| d.join(SESSION)).collect::<Vec<_>>();
        let mut r = Engine::default();
        open_session(&mut r, &file[0].to_string_lossy()).unwrap();
        assert_eq!((file.len(), r.session().sources.len()), (1, 5));
        w.close(false);
        w.flush().unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "closed snapshots are retained");
        assert!(leftovers(&root).is_empty());
        record(&mut e, 0.5);
        w.write(e.session_arc(), None, None);
        drop(w);
        let mut r = Engine::default();
        recover(&mut r, &leftovers(&root)[0]).unwrap();
        assert_eq!(r.session().sources.len(), 6);
    }

    #[test]
    fn a_take_still_recording_at_a_crash_is_placed_on_recovery() {
        use crate::io::Take;
        let dir = folder("autosave-take");
        let root = dir.join("Autosave");
        let mut e = Engine::default();
        let track =
            soundcraft_model::TrackId(e.execute("track.new", &json!({"name": "Vox", "format": "mono"})).unwrap()["tracks"][0].as_u64().unwrap());
        let take = root.join(sanitize_name(&e.session().name)).join("Audio Files/Vox_01.wav");
        std::fs::create_dir_all(take.parent().unwrap()).unwrap();
        let audio: Vec<u8> = (0..480).flat_map(|i| (i as f32 / 1000.0).to_le_bytes()).collect();
        std::fs::write(&take, [soundcraft_audio_io::wav_float_header(1, 48_000, 100).unwrap(), audio].concat()).unwrap();
        let plan = Recording { start: 48_000, pass: None, punch: None, rate: 48_000, takes: vec![Take { track, path: take, channels: 1 }] };
        let clips = |e: &Engine| e.session().track(track).unwrap().clips().iter().map(|c| (c.start, c.length)).collect::<Vec<_>>();
        let w = Autosaver::new(root.clone());
        w.write(e.session_arc(), None, Some(plan.clone()));
        drop(w);
        let left = leftovers(&root);
        assert!(left[0].recording);
        let mut r = Engine::default();
        recover(&mut r, &left[0]).unwrap();
        assert_eq!(clips(&r), [(48_000, 480)], "all of the take, its last second included");
        discard(&left[0]);
        let w = Autosaver::new(root.clone());
        w.write(r.session_arc(), None, Some(plan));
        drop(w);
        let mut again = Engine::default();
        recover(&mut again, &leftovers(&root)[0]).unwrap();
        assert_eq!((clips(&again), again.session().sources.len()), (vec![(48_000, 480)], 1), "placed once");
    }

    #[test]
    fn recovery_never_writes_a_file_that_isnt_a_take_in_a_take_folder() {
        use crate::io::Take;
        let dir = folder("autosave-hostile-take");
        let root = dir.join("Autosave");
        let mut e = Engine::default();
        let track =
            soundcraft_model::TrackId(e.execute("track.new", &json!({"name": "Vox", "format": "mono"})).unwrap()["tracks"][0].as_u64().unwrap());
        let takes = root.join(sanitize_name(&e.session().name)).join("Audio Files");
        std::fs::create_dir_all(&takes).unwrap();
        let wav = [soundcraft_audio_io::wav_float_header(1, 48_000, 0).unwrap(), vec![0; 4_000]].concat();
        let text = b"not audio at all, but long enough to be mistaken for a header if nobody looked at it ......".repeat(4);
        let files =
            [(takes.join("notes.wav"), text.clone()), (dir.join("Elsewhere/Vox_01.wav"), wav.clone()), (takes.join("../Vox_02.wav"), wav.clone())];
        for (path, bytes) in &files {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        // Only the symlink case below adds to it, and symlinks are a Unix-only part of the test.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut paths: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
        #[cfg(unix)]
        {
            let link = takes.join("Vox_03.wav");
            std::os::unix::fs::symlink(&files[1].0, &link).unwrap();
            paths.push(link);
        }
        let plan = Recording {
            start: 0,
            pass: None,
            punch: None,
            rate: 48_000,
            takes: paths.iter().map(|p| Take { track, path: p.clone(), channels: 1 }).collect(),
        };
        let w = Autosaver::new(root.clone());
        w.write(e.session_arc(), None, Some(plan));
        drop(w);
        let mut r = Engine::default();
        let skipped = recover(&mut r, &leftovers(&root)[0]).unwrap();
        assert_eq!(skipped.len(), paths.len(), "{skipped:?}");
        assert!(r.session().sources.is_empty());
        for (path, bytes) in &files {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "{} untouched", path.display());
        }
    }

    #[test]
    fn unsaved_work_stays_recoverable_until_dismissed_after_versioned_save() {
        let dir = folder("autosave-leftovers");
        let path = dir.join("Song.scraft").to_string_lossy().into_owned();
        let root = dir.join("Autosave");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::default();
        crate::io::save_session(&mut e, &path).unwrap();
        record(&mut e, 0.5);
        let mut a = Autosave::create(&root, "Song").unwrap();
        a.write(e.session(), e.path.as_deref(), None).unwrap();
        assert!(leftovers(&root).is_empty());
        drop(a);
        let left = leftovers(&root);
        assert_eq!((left.len(), left[0].name.as_str(), left[0].origin.as_deref()), (1, "Song", Some(path.as_str())));
        let mut r = Engine::default();
        recover(&mut r, &left[0]).unwrap();
        assert!(r.is_dirty());
        let original = std::fs::read(&path).unwrap();
        r.execute("session.save", &json!({})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original, "the original document stays intact");
        assert_ne!(r.path.as_deref(), Some(path.as_str()));
        assert_eq!(leftovers(&root).len(), 1, "a retained recovery entry stays available until explicitly dismissed");
        discard(&left[0]);
        assert!(leftovers(&root).is_empty());
        assert!(left[0].dir.exists(), "outdated snapshots stay intact");
        let mut reopened = Engine::default();
        open_session(&mut reopened, r.path.as_deref().unwrap()).unwrap();
        assert_eq!(levels(&reopened), [0.5]);
        Autosave::create(&root, "Song").unwrap().discard();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2, "old and newly closed folders stay intact");
        assert!(leftovers(&root).is_empty());
    }
}

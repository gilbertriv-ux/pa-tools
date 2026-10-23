//! AAF and OMF interchange for Pa Tools sessions.
//!
//! - **AAF** (`.aaf`, the Edit Protocol over structured storage): [`aaf`]
//! - **OMF Interchange 2.0** (`.omf`, a Bento container) for audio post: [`omf`]
//!
//! [`export`] turns a [`Session`] into a hand-off file: audio and video tracks as slots of one
//! composition, clips with their source offsets, clip gain and gain envelopes, fades and crossfades
//! as transitions, memory locations as markers, the session start timecode and sample rate. The audio
//! is referenced by path, or embedded ([`MediaMode`]). [`import`] reads the same structures (and the
//! common variations other applications write) into a standalone session fragment that
//! [`merge_into`] adds to an open session. Everything the files can say that a session cannot, and
//! the other way round, is listed in the [`Report`] rather than refused.
//!
//! The crate does no file I/O and no audio decoding. Media references are strings (absolute paths,
//! or paths resolved against [`ImportOptions::base_dir`]); the engine probes, loads and relinks them.
//! Embedded audio comes back as WAV bytes ([`ExtractedMedia`]) for the engine to write. Files are
//! untrusted input: every size, count and depth read from one is checked against limits derived from
//! the file's length before anything is allocated, and nothing here panics.
//!
//! All time inside the crate is exact on [`ticks::Tick`]s (254 016 000 000 per second), which divide
//! every common frame and sample duration, so conversions between frames, samples and edit units
//! do not drift.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod aaf;
pub mod omf;

mod common;
mod comp;
mod session;
mod tags;
mod ticks;
mod wav;

use serde::{Deserialize, Serialize};
use soundcraft_model::Session;
use soundcraft_time::Samples;

pub use common::{file_url_to_path, path_to_file_url, resolve_path};
pub use session::{ExtractedMedia, Imported, MAX_EMBED_BYTES, Merged};

/// An interchange format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    /// Advanced Authoring Format (AAF Edit Protocol, structured storage).
    Aaf,
    /// OMF Interchange 2.0 (Bento container), sound only.
    Omf,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Aaf, Format::Omf];

    pub fn name(self) -> &'static str {
        match self {
            Format::Aaf => "AAF",
            Format::Omf => "OMF",
        }
    }

    /// Default file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Format::Aaf => "aaf",
            Format::Omf => "omf",
        }
    }

    /// Format for a file extension (case-insensitive, with or without the dot).
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "aaf" => Some(Format::Aaf),
            "omf" | "omfi" => Some(Format::Omf),
            _ => None,
        }
    }
}

/// Sniff the format of a document from its bytes, using the file extension as a hint.
pub fn detect(bytes: &[u8], extension: Option<&str>) -> Option<Format> {
    if aaf::sniff(bytes) {
        return Some(Format::Aaf);
    }
    if omf::sniff(bytes) && extension.is_none_or(|e| matches!(Format::from_extension(e), None | Some(Format::Omf))) {
        return Some(Format::Omf);
    }
    None
}

/// Error from an importer or exporter. Unsupported *features* never error; they go in the [`Report`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{format} parse error: {message}")]
    Parse { format: &'static str, message: String },
    #[error("the document contains no composition")]
    Empty,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Severity of a report entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Level {
    /// Informational (an assumption the importer made).
    Info,
    /// Something was dropped or approximated.
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub level: Level,
    pub message: String,
    /// How many times this message occurred.
    pub count: u32,
}

/// What an import/export could not represent exactly. Identical messages are merged with a count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub entries: Vec<ReportEntry>,
}

/// The most distinct messages a report keeps: a hostile file cannot grow it without bound.
const MAX_REPORT_ENTRIES: usize = 200;

impl Report {
    pub fn push(&mut self, level: Level, message: impl Into<String>) {
        let message = message.into();
        if let Some(e) = self.entries.iter_mut().find(|e| e.level == level && e.message == message) {
            e.count = e.count.saturating_add(1);
        } else if self.entries.len() < MAX_REPORT_ENTRIES {
            self.entries.push(ReportEntry { level, message, count: 1 });
        }
    }
    pub fn warn(&mut self, message: impl Into<String>) {
        self.push(Level::Warning, message);
    }
    pub fn info(&mut self, message: impl Into<String>) {
        self.push(Level::Info, message);
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn warnings(&self) -> impl Iterator<Item = &ReportEntry> {
        self.entries.iter().filter(|e| e.level == Level::Warning)
    }
    pub fn has_warnings(&self) -> bool {
        self.warnings().next().is_some()
    }
    /// Whether any entry contains `needle` (case-insensitive).
    pub fn mentions(&self, needle: &str) -> bool {
        let n = needle.to_ascii_lowercase();
        self.entries.iter().any(|e| e.message.to_ascii_lowercase().contains(&n))
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for e in &self.entries {
            let lvl = match e.level {
                Level::Info => "info",
                Level::Warning => "warning",
            };
            if e.count > 1 {
                writeln!(f, "{lvl}: {} (x{})", e.message, e.count)?;
            } else {
                writeln!(f, "{lvl}: {}", e.message)?;
            }
        }
        Ok(())
    }
}

/// Import options.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportOptions {
    /// Directory relative media paths are resolved against (normally the document's directory).
    pub base_dir: Option<String>,
    /// The sample rate to import at, normally the open session's: positions in the result are
    /// samples at this rate. Default: the rate the file's composition says.
    pub sample_rate: Option<u32>,
    /// Name of the session fragment (default: the composition's).
    pub name: Option<String>,
    /// Which top-level composition to import when a file has several (0-based; default the first).
    pub composition: usize,
}

/// Whether media is referenced or embedded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaMode {
    /// Reference the session's files by path.
    #[default]
    Link,
    /// Write the audio the clips use (plus handles) into the document. Needs the audio loaded in
    /// the session (`Session::pool`); a source without it is linked, and the report says so.
    Embed,
}

/// Export options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportOptions {
    /// Composition name (default: the session name).
    pub name: Option<String>,
    pub media: MediaMode,
    /// Extra audio before and after every used range when embedding, in milliseconds.
    pub handles_ms: u32,
    /// Split multichannel tracks into one mono slot per channel (and embed one mono essence each).
    pub breakout_to_mono: bool,
    /// Bit depth of embedded audio: 16, 24 or 32.
    pub bits: u16,
    /// AAF: write 512-byte sectors (compound file version 3) instead of 4096-byte sectors.
    pub small_sectors: bool,
    /// AAF: write the video tracks (OMF has no picture).
    pub include_video: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { name: None, media: MediaMode::Link, handles_ms: 1000, breakout_to_mono: false, bits: 24, small_sectors: false, include_video: true }
    }
}

/// Export `session` as `format`. Returns the file's bytes and what could not be represented.
pub fn export(session: &Session, format: Format, opts: &ExportOptions) -> Result<(Vec<u8>, Report)> {
    let mut report = Report::default();
    let doc = session::to_document(session, format, opts, &mut report)?;
    let bytes = match format {
        Format::Aaf => aaf::write_document(&doc, opts.small_sectors, &mut report)?,
        Format::Omf => {
            if doc.compositions.first().is_some_and(|c| !c.markers.is_empty()) {
                report.info("markers are not written to OMF");
            }
            omf::write_document(&doc, &mut report)?
        }
    };
    Ok((bytes, report))
}

/// Import a document into a standalone session fragment.
pub fn import(bytes: &[u8], format: Format, opts: &ImportOptions) -> Result<(Imported, Report)> {
    let mut report = Report::default();
    let doc = match format {
        Format::Aaf => aaf::read_document(bytes, &mut report)?,
        Format::Omf => omf::read_document(bytes, &mut report)?,
    };
    let imported = session::from_document(doc, opts, &mut report)?;
    Ok((imported, report))
}

/// Add an imported fragment to `target`, shifted by `at` samples, with fresh ids: its tracks (before
/// any master), sources, videos and markers. The fragment must be at the target's sample rate (pass
/// [`ImportOptions::sample_rate`] when importing). The caller then writes the extracted media and
/// loads the new sources.
pub fn merge_into(target: &mut Session, fragment: Imported, at: Samples) -> Result<Merged> {
    session::merge(target, fragment, at)
}

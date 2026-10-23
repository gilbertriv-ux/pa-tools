//! The mapping between a Pa Tools session and the composition model of [`crate::comp`].
//!
//! | Session | Composition |
//! |---|---|
//! | sample rate, frame rate, session start timecode | composition edit rate, timecode slot |
//! | audio track (active playlist) | sound slot; one per channel when broken out to mono |
//! | video track | picture slot |
//! | clip: source, offset, start, length | source clip: master mob, start, length |
//! | clip gain and gain envelope | `MonoAudioGain` with a constant or varying amplitude |
//! | fade in / fade out at a clip edge | transition between the clip and a filler |
//! | two clips that overlap (a crossfade) | transition between the two clips |
//! | memory locations | comment markers |
//!
//! Positions are samples at the session rate in the session and [`Tick`]s in the composition; both
//! conversions are exact for every sample rate that divides 254 016 000 000, and round to the
//! nearest sample otherwise.
//!
//! Not mapped, and said so in the [`Report`]: MIDI, instrument, aux, master, VCA and folder tracks,
//! mixer settings and automation, plug-ins, playlists other than the active one, time stretch,
//! muted clips (written as gaps), nested compositions, effects other than gain and dissolves.

use std::collections::{BTreeMap, HashMap};

use soundcraft_audio_io::FileFormat;
use soundcraft_model::{
    ChannelFormat, Clip, ClipContent, ClipId, Fade, FadeShape, MarkerKind, Session, Source as PtSource, SourceId, Track, TrackKind, VideoSource,
};
use soundcraft_time::{FrameRate as PtRate, MAX_POSITION, SampleRate, Samples};

use crate::common::{db_to_gain, file_stem, gain_to_db, resolve_path};
use crate::comp::{CClip, CItem, CKind, CMarker, CTrack, CTransition, Composition, Document, Gain, MAX_TIME, Source, ticks_to_units};
use crate::ticks::{FrameRate, TICKS_PER_SECOND, Tick};
use crate::{Error, ExportOptions, Format, ImportOptions, MediaMode, Report, Result};

/// Embedded audio larger than this in total is refused (AAF streams and OMF positions are 32-bit
/// in practice, and the copy lives in memory next to the session's).
pub const MAX_EMBED_BYTES: u64 = 1 << 31;
/// A gain envelope segment steeper than this many dB is written as several linear pieces, because
/// the file interpolates amplitude linearly and the session interpolates decibels.
const ENVELOPE_STEP_DB: f32 = 3.0;
/// The most pieces one envelope segment is split into.
const ENVELOPE_MAX_PIECES: usize = 16;

// ---------------------------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------------------------

/// The tick of a sample position at `rate` Hz.
pub(crate) fn to_tick(samples: Samples, rate: u32) -> Tick {
    Tick::from_units(samples, i64::from(rate.max(1))).clamp(-MAX_TIME, MAX_TIME)
}

/// The sample nearest to a tick at `rate` Hz, within the positions a session accepts.
pub(crate) fn to_samples(t: Tick, rate: u32) -> Samples {
    ticks_to_units(t, i64::from(rate.max(1)), 1).0.clamp(-MAX_POSITION, MAX_POSITION)
}

/// The composition frame rate (and drop-frame flag) of a session frame rate.
fn comp_rate(r: PtRate) -> (FrameRate, bool) {
    let (n, d) = r.rational();
    (FrameRate::new(n, d), r.is_drop())
}

/// The session frame rate nearest to a composition's (the drop-frame flag breaks ties).
fn session_rate(r: FrameRate, drop: bool, report: &mut Report) -> PtRate {
    let r = r.sane();
    let target = r.num as f64 / r.den as f64;
    let best = PtRate::ALL
        .into_iter()
        .min_by_key(|c| {
            let (n, d) = c.rational();
            (((n as f64 / d as f64 - target).abs() * 1e6) as u64, c.is_drop() != drop)
        })
        .unwrap_or_default();
    let (n, d) = best.rational();
    if (n as f64 / d as f64 - target).abs() > 0.01 {
        report.info(format!("the frame rate {target:.3} fps is not one of the session's rates; {} was used", best.label()));
    }
    best
}

/// The frame whose start is nearest to the sample position `at` (rounding to samples can leave the
/// start of a frame a sample before the exact boundary, which flooring would lose).
fn start_frame(rate: PtRate, at: Samples, sr: SampleRate) -> i64 {
    let f = rate.frames_at(at, sr);
    [f, f.saturating_add(1)].into_iter().min_by_key(|&c| (rate.samples_at_frame(c, sr) - at).abs()).unwrap_or(f)
}

/// The id written for a fade shape.
pub(crate) fn shape_id(s: FadeShape) -> &'static str {
    match s {
        FadeShape::Linear => "linear",
        FadeShape::EqualPower => "equal_power",
        FadeShape::SCurve => "s_curve",
        FadeShape::Exponential => "exponential",
        FadeShape::Logarithmic => "logarithmic",
    }
}

/// The fade shape of an id written by this crate or by the older writer (`constant_power`,
/// `constant_gain`); anything else is the default, equal power.
pub(crate) fn shape_of(id: &str) -> FadeShape {
    match id {
        "linear" | "constant_gain" => FadeShape::Linear,
        "s_curve" => FadeShape::SCurve,
        "exponential" | "exponential_fade" => FadeShape::Exponential,
        "logarithmic" => FadeShape::Logarithmic,
        _ => FadeShape::EqualPower,
    }
}

fn hex_color(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn file_format_of(path: &str) -> FileFormat {
    match crate::common::extension(path).as_str() {
        "wav" | "wave" | "bwf" => FileFormat::Wav,
        "aif" | "aiff" | "aifc" => FileFormat::Aiff,
        "flac" => FileFormat::Flac,
        "mp3" => FileFormat::Mp3,
        "ogg" | "oga" | "opus" => FileFormat::Ogg,
        "m4a" | "aac" => FileFormat::Aac,
        "caf" => FileFormat::Caf,
        _ => FileFormat::Other,
    }
}

fn channel_format(n: u32) -> ChannelFormat {
    ChannelFormat::for_channels(n.clamp(1, 8) as usize)
}

// ---------------------------------------------------------------------------------------------
// Session → composition
// ---------------------------------------------------------------------------------------------

/// One clip placed on a lane of a track, with the crossfades it takes part in.
struct Entry<'a> {
    clip: &'a Clip,
    /// Timeline start and end (samples), the start never before 0.
    start: i64,
    end: i64,
    /// Samples cut from the front of the clip because it started before the session start.
    trim: i64,
    /// Length of the crossfade with the previous / next clip in the lane.
    xin: Option<i64>,
    xout: Option<i64>,
}

impl Entry<'_> {
    fn len(&self) -> i64 {
        self.end - self.start
    }
}

/// Whether `b` can follow `a` in a lane as a crossfade: it starts inside `a`, ends after it, and the
/// crossfade does not collide with the one `a` already starts with.
fn crossfade_ok(a: &Entry, b: &Entry) -> bool {
    a.start < b.start && b.start < a.end && a.end < b.end && a.xin.is_none_or(|ov| a.start.saturating_add(ov) <= b.start)
}

/// Spread clips over lanes: each lane holds clips that do not overlap, except as crossfades.
fn pack<'a>(mut clips: Vec<Entry<'a>>, crossfades: bool) -> Vec<Vec<Entry<'a>>> {
    clips.sort_by_key(|e| (e.start, e.clip.id.0));
    let mut lanes: Vec<Vec<Entry<'a>>> = Vec::new();
    for mut e in clips {
        let target = lanes.iter().position(|lane| match lane.last() {
            Some(last) => last.end <= e.start || (crossfades && crossfade_ok(last, &e)),
            None => true,
        });
        let Some(lane) = target.and_then(|i| lanes.get_mut(i)) else {
            lanes.push(vec![e]);
            continue;
        };
        if let Some(last) = lane.last_mut()
            && last.end > e.start
        {
            let ov = last.end - e.start;
            last.xout = Some(ov);
            e.xin = Some(ov);
        }
        lane.push(e);
    }
    lanes
}

/// What embedding needs to know about one source: the range of frames that clips use.
#[derive(Clone, Copy)]
struct Use {
    lo: i64,
    hi: i64,
}

struct Exporter<'a> {
    s: &'a Session,
    opts: &'a ExportOptions,
    report: &'a mut Report,
    sr: u32,
    rate: FrameRate,
    sources: Vec<Source>,
    memo: HashMap<(u64, bool, Option<u32>), usize>,
    uses: BTreeMap<SourceId, Use>,
    embedded: u64,
    muted: u32,
    stretched: u32,
    bits: u16,
}

/// The composition document of a session.
pub(crate) fn to_document(s: &Session, format: Format, opts: &ExportOptions, report: &mut Report) -> Result<Document> {
    let (rate, drop) = comp_rate(s.frame_rate);
    let bits = match opts.bits {
        0..=16 => 16,
        17..=24 => 24,
        _ => 32,
    };
    let mut ex = Exporter {
        s,
        opts,
        report,
        sr: s.sample_rate.hz(),
        rate,
        sources: Vec::new(),
        memo: HashMap::new(),
        uses: BTreeMap::new(),
        embedded: 0,
        muted: 0,
        stretched: 0,
        bits,
    };
    let want_video = format == Format::Aaf && opts.include_video;
    let mut comp = Composition {
        name: opts.name.clone().unwrap_or_else(|| s.name.clone()),
        rate,
        sample_rate: ex.sr,
        width: 0,
        height: 0,
        start_tc: start_frame(s.frame_rate, s.timecode_start, s.sample_rate),
        drop,
        tracks: Vec::new(),
        markers: Vec::new(),
    };
    if s.frame_rate.samples_at_frame(comp.start_tc, s.sample_rate) != s.timecode_start {
        ex.report.info("the session start timecode is not on a frame and was rounded to one");
    }
    if ex.opts.media == MediaMode::Embed {
        ex.plan_embedding();
    }
    let (mut audio_n, mut video_n) = (0u32, 0u32);
    let mut skipped: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut extra_playlists = 0;
    for t in &s.tracks {
        if t.playlists.len() > 1 {
            extra_playlists += t.playlists.len() - 1;
        }
        match t.kind {
            TrackKind::Audio => ex.audio_track(t, &mut comp, &mut audio_n),
            TrackKind::Video if want_video => ex.video_track(t, &mut comp, &mut video_n),
            TrackKind::Video => *skipped.entry("video").or_default() += 1,
            TrackKind::Midi | TrackKind::Instrument => *skipped.entry("MIDI and instrument").or_default() += 1,
            TrackKind::Aux | TrackKind::Master | TrackKind::Vca | TrackKind::Folder => {
                *skipped.entry("aux, master, VCA and folder").or_default() += 1
            }
        }
    }
    for (kind, n) in skipped {
        ex.report.warn(format!("{n} {kind} track(s) are not exported"));
    }
    if extra_playlists > 0 {
        ex.report.info(format!("{extra_playlists} playlist(s) other than the active ones are not exported"));
    }
    if ex.muted > 0 {
        ex.report.warn(format!("{} muted clip(s) were exported as gaps", ex.muted));
    }
    if ex.stretched > 0 {
        ex.report.warn(format!("{} clip(s) use time stretch, which is not exported (they play at 100%)", ex.stretched));
    }
    for m in &s.markers {
        if m.kind == MarkerKind::None {
            continue;
        }
        let end = if m.kind == MarkerKind::Selection { m.end.max(m.start) } else { m.start };
        comp.markers.push(CMarker {
            start: to_tick(m.start, ex.sr),
            duration: to_tick(end - m.start, ex.sr).max(Tick::ZERO),
            name: m.name.clone(),
            comment: m.comments.clone(),
            color: m.color.map(hex_color),
        });
    }
    // the size of the picture: that of the first movie a video track uses
    if let Some(v) = comp.tracks.iter().filter(|t| t.kind == CKind::Picture).flat_map(|t| &t.items).find_map(|i| match i {
        CItem::Clip(c) => ex.sources.get(c.source),
        _ => None,
    }) {
        comp.width = v.width;
        comp.height = v.height;
    }
    Ok(Document { compositions: vec![comp], sources: ex.sources, nested: Vec::new() })
}

impl Exporter<'_> {
    /// The clips of a playlist that can be written, as placed entries.
    fn entries<'t>(&mut self, t: &'t Track, video: bool) -> Vec<Entry<'t>> {
        let mut out = Vec::new();
        for c in t.clips() {
            let playable = if video { c.is_video() } else { c.is_audio() };
            if !playable {
                if !c.is_audio() && !c.is_video() {
                    self.report.warn("MIDI clips on an audio or video track are not exported");
                }
                continue;
            }
            if c.length <= 0 {
                continue;
            }
            if c.muted {
                self.muted += 1;
                continue;
            }
            if (c.stretch - 1.0).abs() > 1e-9 {
                self.stretched += 1;
            }
            let trim = (-c.start).max(0);
            let start = c.start.max(0);
            let end = c.start.saturating_add(c.length);
            if end <= start {
                continue;
            }
            out.push(Entry { clip: c, start, end, trim, xin: None, xout: None });
        }
        out
    }

    /// Note which frames of each source the audio clips use (for embedding).
    fn plan_embedding(&mut self) {
        let handles = i64::from(self.opts.handles_ms);
        for t in self.s.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            for c in t.clips().iter().filter(|c| !c.muted && c.length > 0) {
                let ClipContent::Audio { source, offset } = c.content else { continue };
                let Some(src) = self.s.source(source) else { continue };
                let h = handles.saturating_mul(i64::from(src.sample_rate)) / 1000;
                let (lo, hi) = (offset.saturating_sub(h).max(0), offset.saturating_add(c.length).saturating_add(h));
                let u = self.uses.entry(source).or_insert(Use { lo, hi });
                u.lo = u.lo.min(lo);
                u.hi = u.hi.max(hi);
            }
        }
    }

    fn audio_track(&mut self, t: &Track, comp: &mut Composition, number: &mut u32) {
        let entries = self.entries(t, false);
        let lanes = pack(entries, true);
        let channels = t.format.channels().max(1) as u32;
        let breakout = self.opts.breakout_to_mono && channels > 1;
        for (li, lane) in lanes.iter().enumerate() {
            let base = if li == 0 { t.name.clone() } else { format!("{} {}", t.name, li + 1) };
            let slots: Vec<Option<u32>> = if breakout { (0..channels).map(Some).collect() } else { vec![None] };
            for ch in slots {
                let items = self.lane_items(lane, CKind::Sound, ch);
                *number += 1;
                let name = match ch {
                    Some(c) if channels == 2 => format!("{base}.{}", if c == 0 { "L" } else { "R" }),
                    Some(c) => format!("{base}.{}", c + 1),
                    None => base.clone(),
                };
                comp.tracks.push(CTrack { kind: CKind::Sound, name, number: *number, channels: if breakout { 1 } else { channels }, items });
            }
        }
        if lanes.is_empty() {
            // an empty track still gets its slot, so the track layout survives
            *number += 1;
            let ch = if breakout { channels } else { 1 };
            for c in 0..ch {
                let name = if breakout { format!("{}.{}", t.name, c + 1) } else { t.name.clone() };
                comp.tracks.push(CTrack {
                    kind: CKind::Sound,
                    name,
                    number: *number,
                    channels: if breakout { 1 } else { channels },
                    items: Vec::new(),
                });
            }
        }
    }

    fn video_track(&mut self, t: &Track, comp: &mut Composition, number: &mut u32) {
        let lanes = pack(self.entries(t, true), false);
        for (li, lane) in lanes.iter().enumerate() {
            let items = self.lane_items(lane, CKind::Picture, None);
            *number += 1;
            let name = if li == 0 { t.name.clone() } else { format!("{} {}", t.name, li + 1) };
            comp.tracks.push(CTrack { kind: CKind::Picture, name, number: *number, channels: 0, items });
        }
    }

    /// The sequence of fillers, clips and transitions of one lane.
    fn lane_items(&mut self, lane: &[Entry], kind: CKind, channel: Option<u32>) -> Vec<CItem> {
        let sr = self.sr;
        let mut out: Vec<CItem> = Vec::new();
        let mut cursor = Tick::ZERO;
        let mut tail = Tick::ZERO;
        for e in lane {
            let src = match e.clip.content {
                ClipContent::Audio { source, .. } if kind == CKind::Sound => self.audio_source(source, channel),
                ClipContent::Video { source, .. } if kind == CKind::Picture => self.video_source(source),
                _ => None,
            };
            let (s, end) = (to_tick(e.start, sr), to_tick(e.end, sr));
            // fades that stay inside the clip next to its crossfades
            let trail_x = e.xout.unwrap_or(0);
            let lead_x = e.xin.unwrap_or(0);
            let fade_in = if kind == CKind::Sound && e.xin.is_none() { e.clip.fade_in.len.clamp(0, (e.len() - trail_x).max(0)) } else { 0 };
            let fade_out =
                if kind == CKind::Sound && e.xout.is_none() { e.clip.fade_out.len.clamp(0, (e.len() - lead_x - fade_in).max(0)) } else { 0 };
            let Some(src) = src else {
                // a clip that cannot be written is a gap, without its fades
                let to = end;
                if to > cursor {
                    out.push(CItem::Filler(to - cursor));
                    cursor = to;
                }
                tail = tail.max(cursor);
                continue;
            };
            if e.xin.is_none() {
                if fade_in > 0 {
                    // a fade-in is a transition from nothing: a filler reaching to its end, then the
                    // transition over that filler's end and the clip's start
                    let len = to_tick(e.start + fade_in, sr) - s;
                    if s + len > cursor {
                        out.push(CItem::Filler(s + len - cursor));
                    }
                    out.push(CItem::Transition(CTransition { len, cut: Tick::ZERO, effect: shape_id(e.clip.fade_in.shape).to_string() }));
                } else if s > cursor {
                    out.push(CItem::Filler(s - cursor));
                }
            }
            let offset = to_tick(e.clip.source_offset().saturating_add(e.trim), self.source_rate(e.clip));
            let start = offset - self.source_offset(src);
            let gain = if kind == CKind::Sound { self.gain_of(e) } else { None };
            out.push(CItem::Clip(CClip { len: end - s, source: src, start, gain, name: e.clip.name.clone() }));
            cursor = end;
            if let Some(ov) = e.xout {
                let ts = to_tick(e.end - ov, sr);
                // (the partner starts at `ts`; the crossfade takes its shape from the later clip)
                let shape = lane_shape(lane, e);
                out.push(CItem::Transition(CTransition { len: end - ts, cut: Tick((end - ts).0 / 2), effect: shape_id(shape).to_string() }));
                cursor = ts;
                tail = tail.max(end);
            } else if fade_out > 0 {
                let len = end - to_tick(e.end - fade_out, sr);
                out.push(CItem::Transition(CTransition { len, cut: len, effect: shape_id(e.clip.fade_out.shape).to_string() }));
                cursor = end - len;
                tail = tail.max(end);
            } else {
                tail = tail.max(end);
            }
        }
        if tail > cursor {
            out.push(CItem::Filler(tail - cursor));
        }
        out
    }

    fn source_rate(&self, c: &Clip) -> u32 {
        match c.content {
            ClipContent::Audio { source, .. } => self.s.source(source).map_or(self.sr, |x| x.sample_rate.max(1)),
            _ => self.sr,
        }
    }

    fn source_offset(&self, index: usize) -> Tick {
        self.sources.get(index).map_or(Tick::ZERO, |s| s.offset)
    }

    /// The gain of a clip: its static gain, or its envelope on top of it.
    fn gain_of(&self, e: &Entry) -> Option<Gain> {
        let c = e.clip;
        let base = if c.gain_db.is_finite() { c.gain_db } else { 0.0 };
        let mut env: Vec<(i64, f32)> =
            c.gain_env.iter().filter(|(_, v)| v.is_finite()).map(|&(t, v)| ((t - e.trim).clamp(0, e.len()), base + v)).collect();
        env.sort_by_key(|p| p.0);
        if env.is_empty() {
            return (base.abs() > 1e-6).then(|| Gain::Constant(db_to_gain(f64::from(base))));
        }
        let mut points: Vec<(Tick, f64)> = Vec::new();
        let mut prev: Option<(i64, f32)> = None;
        for &(t, db) in &env {
            if let Some((pt, pdb)) = prev {
                let pieces = (((db - pdb).abs() / ENVELOPE_STEP_DB).ceil() as usize).clamp(1, ENVELOPE_MAX_PIECES);
                for k in 1..pieces {
                    let f = k as f64 / pieces as f64;
                    let mid_t = pt + ((t - pt) as f64 * f).round() as i64;
                    let mid_db = pdb + (db - pdb) * f as f32;
                    if mid_t > pt && mid_t < t {
                        points.push((to_tick(mid_t, self.sr), db_to_gain(f64::from(mid_db))));
                    }
                }
            }
            points.push((to_tick(t, self.sr), db_to_gain(f64::from(db))));
            prev = Some((t, db));
        }
        Some(Gain::Varying { linear: true, points })
    }

    /// Where the file of `src` is, as written into the document: an absolute path, made so with the
    /// session folder when it has one.
    fn link_path(&mut self, path: &str) -> String {
        if crate::common::is_absolute(path) {
            return path.to_string();
        }
        match &self.s.folder {
            Some(dir) => resolve_path(path, Some(&dir.to_string_lossy())),
            None => {
                self.report.info("the session has no folder, so media paths were written relative as they are");
                path.to_string()
            }
        }
    }

    fn audio_source(&mut self, id: SourceId, channel: Option<u32>) -> Option<usize> {
        let src = self.s.source(id)?;
        let embed_range = if self.opts.media == MediaMode::Embed { self.uses.get(&id).copied() } else { None };
        let audio = embed_range.and_then(|_| self.s.pool.get(id)).cloned();
        let file_channels = u32::from(src.channels).max(1);
        let breakout = self.opts.breakout_to_mono && file_channels > 1;
        let channel = if breakout { Some(channel.unwrap_or(0).min(file_channels - 1)) } else { None };
        let memo_key = (id.0, false, channel);
        if let Some(&i) = self.memo.get(&memo_key) {
            return Some(i);
        }
        let rate = src.sample_rate.max(1);
        let mut s = Source {
            key: format!("audio:{}", id.0),
            name: src.name.clone(),
            kind: CKind::Sound,
            path: Some(self.link_path(&src.path)),
            channel,
            channels: if channel.is_some() { 1 } else { file_channels },
            file_channels,
            width: 0,
            height: 0,
            frame_rate: self.rate,
            sample_rate: rate,
            bits: self.bits,
            length: Tick::from_units(i64::try_from(src.frames).unwrap_or(i64::MAX), i64::from(rate)).clamp(Tick::ZERO, MAX_TIME),
            offset: Tick::ZERO,
            start_tc: None,
            tc_rate: self.rate,
            embedded: None,
            markers: Vec::new(),
            nested: None,
        };
        let mut first_frame = 0i64;
        if let (Some(u), Some(audio)) = (embed_range, audio) {
            match self.embed(&audio.buffer.channels, audio.buffer.sample_rate, u, channel) {
                Ok((pcm, lo, frames, ch)) => {
                    first_frame = lo;
                    s.path = None;
                    s.embedded = Some(pcm);
                    s.channel = None;
                    s.channels = ch;
                    s.file_channels = ch;
                    s.sample_rate = audio.buffer.sample_rate.max(1);
                    s.offset = Tick::from_units(lo, i64::from(s.sample_rate));
                    s.length = Tick::from_units(frames, i64::from(s.sample_rate));
                }
                Err(why) => self.report.warn(format!("\"{}\" was linked instead of embedded: {why}", src.name)),
            }
        } else if self.opts.media == MediaMode::Embed {
            self.report.warn(format!("\"{}\" has no audio loaded and was linked instead of embedded", src.name));
        }
        if src.time_reference > 0 {
            let at = i64::try_from(src.time_reference).unwrap_or(i64::MAX).saturating_add(first_frame);
            s.start_tc = Some(self.rate.frame_at(Tick::from_units(at, i64::from(rate))));
        }
        let i = self.sources.len();
        self.sources.push(s);
        self.memo.insert(memo_key, i);
        Some(i)
    }

    /// 16/24/32-bit little-endian PCM of the used frames of a source (one channel when broken out).
    /// Returns the PCM, the first frame, the frame count and the channel count.
    fn embed(&mut self, chans: &[Vec<f32>], rate: u32, u: Use, channel: Option<u32>) -> std::result::Result<(Vec<u8>, i64, i64, u32), String> {
        let frames = chans.iter().map(Vec::len).min().unwrap_or(0);
        let lo = usize::try_from(u.lo.max(0)).unwrap_or(0).min(frames);
        let hi = usize::try_from(u.hi.max(0)).unwrap_or(0).min(frames).max(lo);
        let picked: Vec<&Vec<f32>> = match channel {
            Some(c) => chans.get(c as usize).into_iter().collect(),
            None => chans.iter().collect(),
        };
        if picked.is_empty() || hi == lo {
            return Err("it has no audio in the used range".into());
        }
        let _ = rate;
        let bps = u64::from(self.bits / 8);
        let size = ((hi - lo) as u64).saturating_mul(picked.len() as u64).saturating_mul(bps);
        if self.embedded.saturating_add(size) > MAX_EMBED_BYTES {
            return Err("there is too much audio to embed".into());
        }
        self.embedded += size;
        let mut pcm = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        for i in lo..hi {
            for ch in &picked {
                let x = f64::from(ch.get(i).copied().unwrap_or(0.0));
                let x = if x.is_finite() { x.clamp(-1.0, 1.0) } else { 0.0 };
                match self.bits {
                    16 => pcm.extend_from_slice(&((x * 32767.0).round() as i16).to_le_bytes()),
                    24 => pcm.extend_from_slice(((x * 8_388_607.0).round() as i32).to_le_bytes().get(..3).unwrap_or(&[0; 3])),
                    _ => pcm.extend_from_slice(&((x * 2_147_483_647.0).round() as i32).to_le_bytes()),
                }
            }
        }
        Ok((pcm, lo as i64, (hi - lo) as i64, picked.len() as u32))
    }

    fn video_source(&mut self, id: SourceId) -> Option<usize> {
        let memo_key = (id.0, true, None);
        if let Some(&i) = self.memo.get(&memo_key) {
            return Some(i);
        }
        let v = self.s.video(id)?;
        let fps = if v.frame_rate.is_finite() && v.frame_rate > 0.0 { v.frame_rate } else { 25.0 };
        let secs = if v.duration.is_finite() && v.duration > 0.0 { v.duration } else { 0.0 };
        let s = Source {
            key: format!("video:{}", id.0),
            name: v.name.clone(),
            kind: CKind::Picture,
            path: Some(v.path.clone()),
            channel: None,
            channels: 0,
            file_channels: 0,
            width: if v.width > 0 { v.width } else { 1920 },
            height: if v.height > 0 { v.height } else { 1080 },
            frame_rate: rate_from_f64(fps),
            sample_rate: 0,
            bits: 0,
            length: Tick::from_units((secs * 1_000_000.0).round().min(1e15) as i64, 1_000_000).clamp(Tick::ZERO, MAX_TIME),
            offset: Tick::ZERO,
            start_tc: None,
            tc_rate: self.rate,
            embedded: None,
            markers: Vec::new(),
            nested: None,
        };
        let i = self.sources.len();
        self.sources.push(s);
        self.memo.insert(memo_key, i);
        Some(i)
    }
}

/// The shape of the crossfade that ends `e`: that of the later clip's fade-in, else of `e`'s fade-out.
fn lane_shape(lane: &[Entry], e: &Entry) -> FadeShape {
    let next = lane.iter().skip_while(|x| x.clip.id != e.clip.id).nth(1);
    match next {
        Some(n) if n.clip.fade_in.len > 0 => n.clip.fade_in.shape,
        _ => e.clip.fade_out.shape,
    }
}

/// The frame rate nearest to `fps` among the standard ones, else the exact thousandth.
pub(crate) fn rate_from_f64(fps: f64) -> FrameRate {
    for (n, d) in [
        (24_000, 1001),
        (24, 1),
        (25, 1),
        (30_000, 1001),
        (30, 1),
        (48_000, 1001),
        (48, 1),
        (50, 1),
        (60_000, 1001),
        (60, 1),
        (120_000, 1001),
        (120, 1),
    ] {
        if (n as f64 / d as f64 - fps).abs() < 0.005 {
            return FrameRate::new(n, d);
        }
    }
    FrameRate::new((fps * 1000.0).round().clamp(1.0, 1e9) as i64, 1000)
}

// ---------------------------------------------------------------------------------------------
// Composition → session
// ---------------------------------------------------------------------------------------------

/// Media the document embeds, to be written by the caller at `path` (relative to the session folder).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedMedia {
    /// The imported source whose file this is.
    pub source: SourceId,
    pub path: String,
    /// A complete WAV file (at the sample rate and bit depth of the embedded audio).
    pub wav: Vec<u8>,
}

/// The result of an import: a standalone session fragment.
#[derive(Clone, Debug)]
pub struct Imported {
    /// Tracks, clips, sources, videos and markers read from the document (ids local to this fragment).
    pub session: Session,
    /// Embedded audio to write next to the session before the sources are loaded.
    pub extracted: Vec<ExtractedMedia>,
}

enum Mapped {
    Audio(SourceId),
    Video(SourceId),
}

/// Build a session fragment from the first (or chosen) composition of a document.
pub(crate) fn from_document(doc: Document, opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let comp = doc.compositions.get(opts.composition).or_else(|| doc.compositions.first()).ok_or(Error::Empty)?;
    if doc.compositions.len() > 1 {
        report.info(format!("the file holds {} compositions; one was imported", doc.compositions.len()));
    }
    let hz = opts.sample_rate.unwrap_or(comp.sample_rate);
    let rate = match SampleRate::new(hz) {
        Ok(r) => r,
        Err(_) => {
            report.warn(format!("the sample rate {hz} Hz is not supported; 48000 Hz was used"));
            SampleRate::HZ_48000
        }
    };
    let sr = rate.hz();
    let mut s = Session::new(opts.name.clone().unwrap_or_else(|| comp.name.clone()), rate);
    s.frame_rate = session_rate(comp.rate, comp.drop, report);
    s.timecode_start = s.frame_rate.samples_at_frame(comp.start_tc, rate).clamp(-MAX_POSITION, MAX_POSITION);

    // media: one source per file (all the channels of a file are one source)
    let mut map: HashMap<usize, Mapped> = HashMap::new();
    let mut extracted = Vec::new();
    let mut groups: Vec<((String, Option<String>), Vec<usize>)> = Vec::new();
    let mut nested = 0usize;
    for (i, src) in doc.sources.iter().enumerate() {
        if src.nested.is_some() {
            nested += 1;
            continue;
        }
        let k = (src.key.clone(), src.path.clone().or_else(|| src.embedded.is_some().then(|| format!("embedded:{i}"))));
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(i),
            None => groups.push((k, vec![i])),
        }
    }
    if nested > 0 {
        report.warn("nested compositions are not imported (their clips were left as gaps)");
    }
    let mut used_paths = std::collections::HashSet::new();
    for ((_, path), members) in &groups {
        let Some(&first_i) = members.first() else { continue };
        let Some(first) = doc.sources.get(first_i) else { continue };
        let id = SourceId(s.alloc());
        let name = if first.name.is_empty() {
            path.as_deref().map(|p| file_stem(p).to_string()).unwrap_or_else(|| "Media".into())
        } else {
            first.name.clone()
        };
        if first.kind == CKind::Picture {
            let resolved = path.as_deref().map(|p| resolve_path(p, opts.base_dir.as_deref())).unwrap_or_default();
            let secs = first.length.0 as f64 / TICKS_PER_SECOND as f64;
            s.videos.push(VideoSource {
                id,
                name,
                path: resolved,
                width: first.width,
                height: first.height,
                frame_rate: first.frame_rate.num as f64 / first.frame_rate.den.max(1) as f64,
                duration: secs,
                codec: String::new(),
                offline: None,
            });
            for &m in members {
                map.insert(m, Mapped::Video(id));
            }
            continue;
        }
        let snd = members.iter().filter_map(|&m| doc.sources.get(m)).filter(|x| x.kind == CKind::Sound);
        let channels = snd.clone().map(|x| x.file_channels.max(x.channel.map_or(0, |c| c + 1))).max().unwrap_or(1).max(1);
        let length = snd.clone().map(|x| x.length).max().unwrap_or(Tick::ZERO);
        let embedded = first.embedded.as_ref().map(|d| (first, d));
        let rel = if let Some((e, data)) = embedded {
            let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
            let base = format!("Audio Files/{}", safe.trim());
            let mut rel = format!("{base}.wav");
            let mut n = 1;
            while !used_paths.insert(rel.to_ascii_lowercase()) {
                n += 1;
                rel = format!("{base}-{n}.wav");
            }
            extracted.push(ExtractedMedia {
                source: id,
                path: rel.clone(),
                wav: crate::wav::wav_file(data, e.file_channels.max(1).min(u32::from(u16::MAX)) as u16, e.sample_rate, e.bits),
            });
            rel
        } else {
            path.as_deref().map(|p| resolve_path(p, opts.base_dir.as_deref())).unwrap_or_default()
        };
        if rel.is_empty() {
            report.warn(format!("\"{name}\" has no media location; imported offline"));
        }
        let time_reference = first.start_tc.map(|f| to_samples(first.tc_rate.tick_of(f), sr)).filter(|&t| t > 0).map_or(0, |t| t as u64);
        s.sources.push(PtSource {
            id,
            name,
            format: if embedded.is_some() { FileFormat::Wav } else { file_format_of(&rel) },
            path: rel,
            channels: channels.min(u32::from(u16::MAX)) as u16,
            frames: to_samples(length, sr).max(0) as u64,
            sample_rate: sr,
            time_reference,
            unsaved: false,
        });
        for &m in members {
            map.insert(m, Mapped::Audio(id));
        }
    }

    if doc.sources.iter().any(|x| x.kind == CKind::Sound && x.channel.is_some_and(|c| c > 0) && x.nested.is_none()) {
        report.warn("a clip cannot select one channel of a multichannel file; clips of a broken-out channel play the first channel of their file");
    }
    // tracks
    for t in &comp.tracks {
        import_track(&mut s, t, &doc.sources, &map, sr, report);
    }
    let media_markers: usize = doc.sources.iter().map(|x| x.markers.len()).sum();
    if media_markers > 0 {
        report.info(format!("{media_markers} marker(s) on media were not imported"));
    }
    for m in &comp.markers {
        let start = to_samples(m.start, sr);
        let end = start.saturating_add(to_samples(m.duration, sr).max(0));
        let kind = if end > start { MarkerKind::Selection } else { MarkerKind::Marker };
        let id = s.add_marker(&m.name, kind, start, end);
        if let Some(loc) = s.markers.iter_mut().find(|x| x.id == id) {
            loc.comments = m.comment.clone();
            loc.color = m.color.as_deref().and_then(parse_color);
        }
    }
    s.sanitize();
    Ok(Imported { session: s, extracted })
}

enum Slot {
    Gap,
    Clip(usize),
    Tr { len: Tick, effect: String },
}

fn import_track(s: &mut Session, t: &CTrack, sources: &[Source], map: &HashMap<usize, Mapped>, sr: u32, report: &mut Report) {
    let video = t.kind == CKind::Picture;
    let kind = if video { TrackKind::Video } else { TrackKind::Audio };
    let format = if video { ChannelFormat::Mono } else { channel_format(t.channels) };
    let tid = s.add_track(kind, format, Some(if t.name.is_empty() { kind.default_name() } else { &t.name }));
    // keep the name from the file (the session would number a repeated one)
    let wanted = if t.name.is_empty() { None } else { Some(t.name.clone()) };

    // lay the items out on the timeline: a transition overlaps what came before it
    let mut pos = Tick::ZERO;
    let mut slots: Vec<Slot> = Vec::new();
    let mut clips: Vec<(Tick, Tick, &CClip)> = Vec::new();
    for it in &t.items {
        if pos.0.abs() > 4 * MAX_TIME.0 {
            report.warn("a track longer than supported was cut short");
            break;
        }
        match it {
            CItem::Filler(l) => {
                pos += *l;
                slots.push(Slot::Gap);
            }
            CItem::Transition(tr) => {
                pos -= tr.len;
                slots.push(Slot::Tr { len: tr.len, effect: tr.effect.clone() });
            }
            CItem::Clip(c) => {
                if matches!(map.get(&c.source), Some(Mapped::Audio(_) | Mapped::Video(_))) && c.len > Tick::ZERO {
                    slots.push(Slot::Clip(clips.len()));
                    clips.push((pos, c.len, c));
                } else {
                    slots.push(Slot::Gap);
                }
                pos += c.len;
            }
        }
    }

    // build the clips (in samples), then fades and crossfades from the transitions
    let mut built: Vec<Clip> = Vec::new();
    for &(at, len, c) in &clips {
        let Some(m) = map.get(&c.source) else { continue };
        let start = to_samples(at, sr).max(0);
        let end = to_samples(at + len, sr);
        let length = (end - start).max(1);
        let offset = to_samples(c.start, sr).max(0);
        let cid = ClipId(s.alloc());
        let fallback = sources.get(c.source).map(|x| x.name.as_str()).unwrap_or("");
        let name = if c.name.is_empty() { fallback.to_string() } else { c.name.clone() };
        let mut clip = match m {
            Mapped::Audio(id) => Clip::audio(cid, name, *id, offset, start, length),
            Mapped::Video(id) => Clip::video(cid, name, *id, offset, start, length),
        };
        if !video && let Some(g) = &c.gain {
            apply_gain(&mut clip, g, sr);
        }
        built.push(clip);
    }
    for (k, slot) in slots.iter().enumerate() {
        let Slot::Tr { len, effect } = slot else { continue };
        if video {
            report.info("video transitions are not imported");
            continue;
        }
        let side = |i: Option<usize>| match i.and_then(|i| slots.get(i)) {
            Some(Slot::Clip(c)) => Some(*c),
            _ => None,
        };
        let before = side(k.checked_sub(1));
        let after = side(Some(k + 1));
        let shape = shape_of(effect);
        let n = to_samples(*len, sr).max(1);
        match (before, after) {
            (Some(a), Some(b)) => {
                crossfade(&mut built, a, b, n, shape);
            }
            (Some(a), None) => {
                if let Some(c) = built.get_mut(a) {
                    c.fade_out = Fade { len: n.min(c.length), shape, curvature: 0.0 };
                }
            }
            (None, Some(b)) => {
                if let Some(c) = built.get_mut(b) {
                    c.fade_in = Fade { len: n.min(c.length), shape, curvature: 0.0 };
                }
            }
            (None, None) => report.warn("a transition between two gaps was dropped"),
        }
    }
    if let Some(tr) = s.track_mut(tid) {
        if let Some(n) = wanted {
            tr.name = n.clone();
            if let Some(p) = tr.playlist_mut() {
                p.name = n;
            }
        }
        if let Some(p) = tr.playlist_mut() {
            p.clips = built.into_iter().filter(|c| c.length > 0).collect();
            p.sort();
        }
    }
}

/// Make the clips `a` and `b` (adjacent, in order) overlap by `n` samples as a crossfade.
fn crossfade(built: &mut [Clip], a: usize, b: usize, n: i64, shape: FadeShape) {
    let (Some(ca), Some(cb)) = (built.get(a).cloned(), built.get(b).cloned()) else { return };
    // the overlap cannot be longer than either clip
    let ov = n.min(ca.length - 1).min(cb.length - 1).max(0);
    let want_start = ca.end() - ov;
    let shift = want_start - cb.start;
    if let Some(cb) = built.get_mut(b) {
        if shift != 0 && ov < n {
            // the clips did not overlap by the full transition: the later one starts where the crossfade does
            cb.start += shift;
            cb.length -= shift;
            if let ClipContent::Audio { offset, .. } | ClipContent::Video { offset, .. } = &mut cb.content {
                *offset += shift;
            }
        }
        cb.fade_in = Fade { len: ov, shape, curvature: 0.0 };
    }
    if let Some(ca) = built.get_mut(a) {
        ca.fade_out = Fade { len: ov, shape, curvature: 0.0 };
    }
}

/// A clip's gain from the document: static gain, or an envelope.
fn apply_gain(clip: &mut Clip, g: &Gain, sr: u32) {
    match g {
        Gain::Constant(a) => clip.gain_db = gain_to_db(*a).clamp(-144.0, 48.0) as f32,
        Gain::Varying { linear, points } => {
            let mut pts: Vec<(i64, f32)> =
                points.iter().map(|(t, a)| (to_samples(*t, sr).clamp(0, clip.length), gain_to_db(*a).clamp(-144.0, 48.0) as f32)).collect();
            pts.sort_by_key(|p| p.0);
            let mut env: Vec<(i64, f32)> = Vec::with_capacity(pts.len());
            for (at, db) in pts {
                if !*linear
                    && let Some(&(prev_at, prev_db)) = env.last()
                    && at - 1 > prev_at
                {
                    // a hold: the previous value lasts until just before this point
                    env.push((at - 1, prev_db));
                }
                env.push((at, db));
            }
            clip.gain_env = if *linear { simplify(env) } else { env };
        }
    }
}

/// Drop envelope points that lie on the decibel-linear line between their neighbours (the pieces an
/// exporter adds so the file's amplitude-linear interpolation follows the session's).
fn simplify(env: Vec<(i64, f32)>) -> Vec<(i64, f32)> {
    let mut out: Vec<(i64, f32)> = Vec::with_capacity(env.len());
    for p in env {
        loop {
            let n = out.len();
            let (Some(&a), Some(&b)) = (n.checked_sub(2).and_then(|i| out.get(i)), n.checked_sub(1).and_then(|i| out.get(i))) else { break };
            if b.0 <= a.0 || p.0 <= b.0 {
                break;
            }
            let f = (b.0 - a.0) as f64 / (p.0 - a.0) as f64;
            let on_line = f64::from(a.1) + (f64::from(p.1) - f64::from(a.1)) * f;
            if (on_line - f64::from(b.1)).abs() < 0.03 {
                out.pop();
            } else {
                break;
            }
        }
        out.push(p);
    }
    out
}

/// Positions (and nothing else) of a fragment shifted by `at` samples, with fresh ids, appended to
/// `target`. See [`crate::merge_into`].
pub(crate) fn merge(target: &mut Session, frag: Imported, at: Samples) -> Result<Merged> {
    let Imported { session: mut f, extracted } = frag;
    if f.sample_rate != target.sample_rate {
        return Err(Error::Other(format!("the fragment is at {} Hz and the session at {} Hz", f.sample_rate.hz(), target.sample_rate.hz())));
    }
    let at = at.clamp(-MAX_POSITION, MAX_POSITION);
    let mut ids: HashMap<u64, u64> = HashMap::new();
    let mut fresh = |t: &mut Session, old: u64| *ids.entry(old).or_insert_with(|| t.alloc());
    let mut out = Merged { tracks: Vec::new(), sources: Vec::new(), extracted: Vec::new() };
    for src in &mut f.sources {
        let new = SourceId(fresh(target, src.id.0));
        src.id = new;
    }
    for v in &mut f.videos {
        v.id = SourceId(fresh(target, v.id.0));
    }
    let remap = |ids: &HashMap<u64, u64>, id: SourceId| SourceId(ids.get(&id.0).copied().unwrap_or(id.0));
    for mut t in f.tracks.drain(..) {
        let new_id = soundcraft_model::TrackId(target.alloc());
        t.id = new_id;
        for p in &mut t.playlists {
            for c in &mut p.clips {
                c.id = ClipId(target.alloc());
                c.start = c.start.saturating_add(at).clamp(-MAX_POSITION, MAX_POSITION);
                match &mut c.content {
                    ClipContent::Audio { source, .. } | ClipContent::Video { source, .. } => *source = remap(&ids, *source),
                    ClipContent::Midi { .. } => {}
                }
            }
        }
        // a track named like one already there gets a number, as a new track would
        t.name = target.unique_track_name(&t.name);
        let pos = target.tracks.iter().position(|x| x.kind == TrackKind::Master).unwrap_or(target.tracks.len());
        target.tracks.insert(pos, t);
        out.tracks.push(new_id);
    }
    out.sources = f.sources.iter().map(|s| s.id).chain(f.videos.iter().map(|v| v.id)).collect();
    target.sources.append(&mut f.sources);
    target.videos.append(&mut f.videos);
    for mut m in f.markers.drain(..) {
        m.id = soundcraft_model::MarkerId(target.alloc());
        m.start = m.start.saturating_add(at).clamp(-MAX_POSITION, MAX_POSITION);
        m.end = m.end.saturating_add(at).clamp(m.start, MAX_POSITION);
        m.number = target.markers.iter().map(|x| x.number).max().unwrap_or(0).saturating_add(1);
        target.markers.push(m);
    }
    target.markers.sort_by_key(|m| m.start);
    for e in extracted {
        out.extracted.push(ExtractedMedia { source: remap(&ids, e.source), path: e.path, wav: e.wav });
    }
    Ok(out)
}

/// What [`crate::merge_into`] added.
#[derive(Clone, Debug, PartialEq)]
pub struct Merged {
    pub tracks: Vec<soundcraft_model::TrackId>,
    /// The new audio and video sources, to be loaded by the engine.
    pub sources: Vec<SourceId>,
    /// Embedded audio to write (ids and paths as in the target session).
    pub extracted: Vec<ExtractedMedia>,
}

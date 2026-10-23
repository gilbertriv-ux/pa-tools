//! [`Movie`]: open an MP4/MOV, report its picture format, and decode frames at times to RGBA.

use crate::convert::{Matrix, Yuv, planar_to_rgba, scale_rgba};
use crate::gop::{Described, GopDecoder};
use crate::isobmff::{self, ByteSource, CodecConfig, Mp4File, TrackKind};
use crate::{Result, VideoError, adapt_av1, adapt_dnx, adapt_h264, adapt_hevc};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// A random-access byte source a [`Movie`] can read from (in-memory bytes, or a file on native).
pub type SharedSource = Arc<dyn ByteSource + Send + Sync>;

/// Largest picture we decode (bounds every allocation derived from the file).
pub(crate) const MAX_PIXELS: u64 = 8192 * 8192;

/// A decoded picture, packed RGBA (8 bits per channel, `width * height * 4` bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Presentation index (0-based, in display order).
    pub index: usize,
    /// Presentation time in seconds from the start of the movie.
    pub time: f64,
}

impl Frame {
    /// Downscaled copy that fits inside `max_w` × `max_h` (aspect kept; never upscales).
    pub fn fit(&self, max_w: u32, max_h: u32) -> Frame {
        let (w, h) = fit_size(self.width, self.height, max_w, max_h);
        if (w, h) == (self.width, self.height) {
            return self.clone();
        }
        Frame {
            width: w,
            height: h,
            rgba: scale_rgba(&self.rgba, self.width as usize, self.height as usize, w as usize, h as usize),
            index: self.index,
            time: self.time,
        }
    }
}

/// The size that fits `w` × `h` inside `max_w` × `max_h`, keeping the aspect ratio (≥ 1 px).
pub fn fit_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (1, 1);
    }
    let s = (f64::from(max_w.max(1)) / f64::from(w)).min(f64::from(max_h.max(1)) / f64::from(h)).min(1.0);
    (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1))
}

/// What the movie's picture is, and whether we can decode it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MovieInfo {
    /// Coded picture size (after cropping), in pixels.
    pub width: u32,
    pub height: u32,
    /// Pixel aspect ratio (1.0 = square pixels).
    pub pixel_aspect: f64,
    /// Length of the picture track in seconds.
    pub duration: f64,
    /// Nominal frame rate (frames per second).
    pub frame_rate: f64,
    pub frame_count: usize,
    /// Short codec id: `h264`, `hevc`, `av1`, `prores`, `dnxhd`, `mjpeg`, …
    pub codec: String,
    /// Human-readable codec description shown on screen (e.g. "H.264 High", "PA ProRez 422 HQ",
    /// "VC-3 (SQ, 10-bit)"); never a third-party product name.
    pub codec_detail: String,
    /// True when SoundCraft can decode this picture (otherwise frame requests return `Err`).
    pub decodable: bool,
    /// The file also has an audio track.
    pub has_audio: bool,
    /// QuickTime (`.mov`) rather than ISO MP4 brand.
    pub quicktime: bool,
}

impl MovieInfo {
    /// Display aspect ratio (width / height, including pixel aspect).
    pub fn display_aspect(&self) -> f64 {
        if self.height == 0 { 16.0 / 9.0 } else { f64::from(self.width) * self.pixel_aspect / f64::from(self.height) }
    }
}

/// How many sync samples a start search may step back over (open GOPs, unreliable sync flags).
pub(crate) const MAX_START_STEPS: usize = 32;

/// How many pictures a decoder may output past the one asked for, with the asked-for one still not
/// among them, before the request gives up on it (a damaged sample, or a picture the stream never
/// outputs). The container's presentation order is what labels each picture, and a decoder's output
/// order does not always agree with it: a B-frame stream whose `ctts` is missing or wrong, or an
/// H.264 stream that understates its reorder depth, outputs pictures whose labels run ahead of the
/// asked-for one before it arrives. How far ahead is the stream's real reorder depth, which is 4 or
/// less in everyday encodes and up to 7 in deep hierarchical GOPs, so the margin is 8. It also bounds
/// the work a missing picture costs: the loop stops about this many pictures (plus the decoder's own
/// hold-back) after the one asked for, not at the end of the movie.
pub(crate) const PAST_PICTURES_BEFORE_GIVING_UP: usize = 8;

/// Builds a [`GopDecoder`] (test movies run on scripted decoders).
#[cfg(test)]
pub(crate) type GopFactory = Arc<dyn Fn() -> Result<Box<dyn GopDecoder>> + Send + Sync>;

/// The track's codec and what is needed to build its decoder. Long-GOP codecs (H.264, HEVC, AV1)
/// go through [`GopDecoder`]; ProRes, Motion JPEG and DNx decode one sample at a time.
enum Codec {
    H264 {
        avcc: Vec<u8>,
    },
    Hevc {
        hvcc: Vec<u8>,
    },
    Av1 {
        config_obus: Vec<u8>,
    },
    ProRes,
    Jpeg,
    Dnx,
    Unsupported(String),
    #[cfg(test)]
    Test(GopFactory),
}

impl Codec {
    /// Decoded forward from a random-access sample, with picture reordering.
    fn is_gop(&self) -> bool {
        match self {
            Codec::H264 { .. } | Codec::Hevc { .. } | Codec::Av1 { .. } => true,
            #[cfg(test)]
            Codec::Test(_) => true,
            Codec::ProRes | Codec::Jpeg | Codec::Dnx | Codec::Unsupported(_) => false,
        }
    }

    /// A fresh decoder for a long-GOP codec.
    fn new_gop(&self) -> Result<Box<dyn GopDecoder>> {
        match self {
            Codec::H264 { avcc } => adapt_h264::open(avcc),
            Codec::Hevc { hvcc } => adapt_hevc::open(hvcc),
            Codec::Av1 { config_obus } => adapt_av1::open(config_obus),
            #[cfg(test)]
            Codec::Test(make) => make(),
            Codec::ProRes | Codec::Jpeg | Codec::Dnx | Codec::Unsupported(_) => Err(VideoError::Unsupported("not a long-GOP codec".into())),
        }
    }
}

/// An adapter's verdict on a track, as the codec to use plus the id and description to report.
fn classified(d: Described, id: &'static str, usable: Codec) -> (Codec, &'static str, String) {
    match d.unsupported {
        None => (usable, id, d.detail),
        Some(why) => (Codec::Unsupported(why), id, d.detail),
    }
}

/// Up to `max` bytes from the start of sample `index` (a codec's header), if they can be read.
fn sample_head(src: &dyn ByteSource, track: &isobmff::Track, index: usize, max: usize) -> Option<Vec<u8>> {
    let s = track.samples.get(index)?;
    let mut buf = vec![0u8; usize::try_from(s.size).ok()?.min(max)];
    src.read_at(s.offset, &mut buf).ok()?;
    Some(buf)
}

/// A packed picture as a [`Frame`], refusing sizes and buffers that do not add up.
fn checked_frame(width: u32, height: u32, rgba: Vec<u8>, index: usize, time: f64) -> Result<Frame> {
    let px = u64::from(width) * u64::from(height);
    if px > MAX_PIXELS {
        return Err(VideoError::Decode("picture too large".into()));
    }
    if px == 0 || rgba.len() as u64 != px * 4 {
        return Err(VideoError::Decode("the decoder produced an empty picture".into()));
    }
    Ok(Frame { width, height, rgba, index, time })
}

/// A decoder fed forward from `start`, and the pictures it has produced but nobody asked for yet.
struct GopState {
    dec: Box<dyn GopDecoder>,
    /// The sample (decode order) the decoder was started at.
    start: usize,
    /// Next sample (decode order) to feed.
    next: usize,
    /// Last presentation index output, if any.
    last_out: Option<usize>,
    /// Output pictures not requested yet (presentation index → picture).
    ready: BTreeMap<usize, Yuv>,
}

/// Most bytes of decoded RGBA pictures the full-size frame cache keeps (1 GiB: Gil, decision 5,
/// 2026-10-23). The cache is an LRU by bytes: about 129 frames at 1080p, 32 at 4K UHD, 8 at 8K UHD
/// and 4 at 8192 x 8192. It is separate from the decoder's own working-memory budget. See
/// [`Movie::set_cache_bytes`].
pub const DEFAULT_CACHE_BYTES: usize = 1 << 30;

/// Most frames the full-size cache keeps. This is a ceiling for tiny pictures (the lookup is a linear
/// scan), not the bound: the byte budget is, from 480p up. [`Movie::set_cache_frames`] lowers it.
pub const DEFAULT_CACHE_FRAMES: usize = 512;

/// Most bytes the thumbnail cache keeps (a timeline thumbnail is a few tens of kilobytes, so this
/// only bites if someone asks for very tall ones).
const THUMB_CACHE_BYTES: usize = 256 << 20;

/// Most pictures, and most bytes of them, a running decoder keeps for requests that have not come
/// yet. In practice it is the reorder depth (a few pictures); the byte bound is for 8K pictures, which
/// are about 100 MB each in 10-bit 4:2:0.
const READY_MAX_PICTURES: usize = 32;
const READY_MAX_BYTES: usize = 256 << 20;

/// A tiny LRU of decoded frames, bounded by count and by bytes of picture data. The newest entry is
/// always kept, even when it alone is over the byte bound. (Callers that still hold an evicted frame
/// keep it alive; the bound covers what the cache itself holds.)
struct Lru<K: PartialEq + Copy> {
    cap: usize,
    byte_cap: usize,
    bytes: usize,
    items: VecDeque<(K, Arc<Frame>)>,
}

impl<K: PartialEq + Copy> Lru<K> {
    fn new(cap: usize, byte_cap: usize) -> Self {
        Lru { cap: cap.max(1), byte_cap: byte_cap.max(1), bytes: 0, items: VecDeque::new() }
    }
    fn get(&mut self, k: K) -> Option<Arc<Frame>> {
        let i = self.items.iter().position(|(x, _)| *x == k)?;
        let item = self.items.remove(i)?;
        let f = Arc::clone(&item.1);
        self.items.push_back(item);
        Some(f)
    }
    fn put(&mut self, k: K, f: Arc<Frame>) {
        if let Some(i) = self.items.iter().position(|(x, _)| *x == k)
            && let Some((_, old)) = self.items.remove(i)
        {
            self.bytes = self.bytes.saturating_sub(old.rgba.len());
        }
        self.bytes = self.bytes.saturating_add(f.rgba.len());
        self.items.push_back((k, f));
        self.trim();
    }
    /// Drop the oldest entries until the count and byte bounds hold (the newest one stays).
    fn trim(&mut self) {
        while self.items.len() > 1 && (self.items.len() > self.cap || self.bytes > self.byte_cap) {
            let Some((_, old)) = self.items.pop_front() else { break };
            self.bytes = self.bytes.saturating_sub(old.rgba.len());
        }
    }
    fn contains(&self, k: K) -> bool {
        self.items.iter().any(|(x, _)| *x == k)
    }
}

/// Drop the highest-numbered pictures of `ready` until at most `max_pictures` and `max_bytes` are
/// held. The lowest-numbered one always stays (it is the next one a sequential reader asks for).
fn trim_ready(ready: &mut BTreeMap<usize, Yuv>, max_pictures: usize, max_bytes: usize) {
    let mut bytes: usize = ready.values().fold(0, |a, y| a.saturating_add(y.byte_len()));
    while ready.len() > 1 && (ready.len() > max_pictures || bytes > max_bytes) {
        let Some((_, y)) = ready.pop_last() else { break };
        bytes = bytes.saturating_sub(y.byte_len());
    }
}

/// An opened movie: container index plus decoder state and frame caches.
pub struct Movie {
    src: SharedSource,
    mp4: Mp4File,
    track: usize,
    info: MovieInfo,
    codec: Codec,
    /// Presentation index → sample index (decode order).
    order: Vec<usize>,
    /// Sample index → presentation index.
    rank: Vec<usize>,
    /// Presentation index → time in seconds.
    times: Vec<f64>,
    gop: Option<GopState>,
    cache: Lru<usize>,
    thumbs: Lru<(usize, u32)>,
    /// Decoded frames since open (statistics for tests and the UI).
    pub decoded: u64,
}

impl std::fmt::Debug for Movie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Movie").field("info", &self.info).finish_non_exhaustive()
    }
}

impl Movie {
    /// Open a movie held in memory.
    pub fn open(bytes: impl Into<Arc<[u8]>>) -> Result<Movie> {
        let b: Arc<[u8]> = bytes.into();
        Movie::open_source(Arc::new(b))
    }

    /// Open a movie from any random-access source (e.g. a `std::fs::File` on native targets, so
    /// large movies are not read into memory).
    pub fn open_source(src: SharedSource) -> Result<Movie> {
        let mp4 = isobmff::open(&*src).map_err(|e| VideoError::Container(e.to_string()))?;
        let track = mp4
            .tracks
            .iter()
            .position(|t| t.kind == TrackKind::Video && t.enabled && !t.samples.is_empty())
            .or_else(|| mp4.tracks.iter().position(|t| t.kind == TrackKind::Video && !t.samples.is_empty()))
            .ok_or(VideoError::NoVideoTrack)?;
        let has_audio = mp4.tracks.iter().any(|t| t.kind == TrackKind::Audio && !t.samples.is_empty());
        let t = mp4.tracks.get(track).ok_or(VideoError::NoVideoTrack)?;
        let ts = f64::from(t.timescale.max(1));
        let n = t.samples.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (t.samples.get(i).map_or(0, |s| s.pts), i));
        let mut rank = vec![0usize; n];
        for (p, &i) in order.iter().enumerate() {
            if let Some(r) = rank.get_mut(i) {
                *r = p;
            }
        }
        let times: Vec<f64> = order.iter().map(|&i| t.presentation_pts(i).unwrap_or(0) as f64 / ts).collect();
        let first = times.first().copied().unwrap_or(0.0);
        let end = order.iter().filter_map(|&i| Some(t.presentation_pts(i)?.saturating_add(i64::from(t.samples.get(i)?.duration)))).max().unwrap_or(0)
            as f64
            / ts;
        let duration = (end - first.min(0.0)).max(0.0);
        // Nominal rate: the most common sample duration.
        let mut durs: Vec<u32> = t.samples.iter().map(|s| s.duration).filter(|&d| d > 0).collect();
        durs.sort_unstable();
        let mut best = (0u32, 0usize);
        let mut k = 0;
        while k < durs.len() {
            let d = durs.get(k).copied().unwrap_or(0);
            let run = durs.iter().skip(k).take_while(|&&x| x == d).count().max(1);
            if run > best.1 {
                best = (d, run);
            }
            k += run;
        }
        let frame_rate = if best.0 > 0 {
            ts / f64::from(best.0)
        } else if duration > 0.0 {
            n as f64 / duration
        } else {
            0.0
        };
        let vp = t.video();
        let (mut width, mut height) = vp.map_or((t.width >> 16, t.height >> 16), |v| (u32::from(v.width), u32::from(v.height)));
        if width == 0 || height == 0 {
            (width, height) = (t.width >> 16, t.height >> 16);
        }
        let pixel_aspect = vp.and_then(|v| v.pixel_aspect).filter(|&(h, v)| h > 0 && v > 0).map_or(1.0, |(h, v)| f64::from(h) / f64::from(v));
        let (codec, name, detail) = match t.codec() {
            Some(CodecConfig::Avc(c)) => {
                let (ok, prof) = match c.profile {
                    66 => (true, "Baseline"),
                    77 => (true, "Main"),
                    88 => (true, "Extended"),
                    100 => (true, "High"),
                    110 => (false, "High 10"),
                    122 => (false, "High 4:2:2"),
                    244 => (false, "High 4:4:4"),
                    _ => (true, "?"),
                };
                let detail = format!("H.264 {prof}");
                if ok {
                    (Codec::H264 { avcc: c.to_bytes() }, "h264", detail)
                } else {
                    (Codec::Unsupported(format!("{detail} (only 8-bit 4:2:0 H.264 is supported)")), "h264", detail)
                }
            }
            Some(CodecConfig::ProRes { fourcc }) => {
                let f = fourcc.0;
                let flavour = match &f {
                    b"apco" => "422 Proxy",
                    b"apcs" => "422 LT",
                    b"apcn" => "422",
                    b"apch" => "422 HQ",
                    b"ap4h" => "4444",
                    b"ap4x" => "4444 XQ",
                    _ => "",
                };
                (Codec::ProRes, "prores", format!("PA ProRez {flavour}").trim_end().to_string())
            }
            Some(CodecConfig::Jpeg { fourcc }) if &fourcc.0 != b"mjpb" => (Codec::Jpeg, "mjpeg", "Motion JPEG".to_string()),
            Some(CodecConfig::Hevc(c)) => classified(adapt_hevc::describe(c), "hevc", Codec::Hevc { hvcc: c.to_bytes() }),
            Some(CodecConfig::Av1(c)) => classified(adapt_av1::describe(c), "av1", Codec::Av1 { config_obus: c.config_obus.clone() }),
            Some(CodecConfig::Dnx { fourcc }) => {
                let head = sample_head(&*src, t, 0, 1024);
                classified(adapt_dnx::describe(fourcc.0, head.as_deref()), "dnxhd", Codec::Dnx)
            }
            Some(other) => {
                let d = Described::not_yet(&other.name().to_ascii_uppercase());
                (Codec::Unsupported(d.unsupported.unwrap_or_default()), other.name(), d.detail)
            }
            None => (Codec::Unsupported("the video track has no sample description".into()), "unknown", "unknown".into()),
        };
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(VideoError::Unsupported(format!("{width}x{height} is larger than this app decodes")));
        }
        let info = MovieInfo {
            width,
            height,
            pixel_aspect,
            duration,
            frame_rate,
            frame_count: n,
            codec: name.to_string(),
            codec_detail: detail,
            decodable: !matches!(codec, Codec::Unsupported(_)),
            has_audio,
            quicktime: mp4.is_quicktime,
        };
        Ok(Movie {
            src,
            mp4,
            track,
            info,
            codec,
            order,
            rank,
            times,
            gop: None,
            cache: Lru::new(DEFAULT_CACHE_FRAMES, DEFAULT_CACHE_BYTES),
            thumbs: Lru::new(256, THUMB_CACHE_BYTES),
            decoded: 0,
        })
    }

    pub fn info(&self) -> &MovieInfo {
        &self.info
    }

    /// Test seam: decode this movie's samples with the DNx adapter (which has no decoder yet).
    #[cfg(test)]
    pub(crate) fn with_dnx_adapter(mut self) -> Movie {
        self.codec = Codec::Dnx;
        self.info.decodable = true;
        self
    }

    /// Test seam: decode this movie's samples with a scripted long-GOP decoder.
    #[cfg(test)]
    pub(crate) fn with_test_gop(mut self, make: GopFactory) -> Movie {
        self.codec = Codec::Test(make);
        self.info.decodable = true;
        self
    }

    /// Frames kept decoded (full size). Thumbnails have their own cache. The byte bound of
    /// [`Movie::set_cache_bytes`] applies as well, so a large count of large frames is still held to it.
    pub fn set_cache_frames(&mut self, n: usize) {
        self.cache.cap = n.clamp(1, 512);
        self.cache.trim();
    }

    /// Most bytes of decoded RGBA pictures the full-size frame cache keeps (default
    /// [`DEFAULT_CACHE_BYTES`], 1 GiB). The oldest frames are dropped first, and the newest frame
    /// always stays, so a single picture larger than the bound is still cached. This bounds the cache
    /// only: decoder state, the pictures a running decoder holds for coming requests, and frames the
    /// caller keeps are on top (see `docs/video-codecs.md`).
    pub fn set_cache_bytes(&mut self, bytes: usize) {
        self.cache.byte_cap = bytes.max(1);
        self.cache.trim();
    }

    /// Bytes of picture data the full-size frame cache holds now.
    pub fn cached_bytes(&self) -> usize {
        self.cache.bytes
    }

    /// The frame shown at `secs` (presentation index), or `None` before the first / after the
    /// last frame.
    pub fn frame_index_at(&self, secs: f64) -> Option<usize> {
        if !secs.is_finite() || secs < 0.0 || secs >= self.info.duration {
            return None;
        }
        // Allow for rounding of `secs` computed from sample positions.
        let k = self.times.partition_point(|&t| t <= secs + 1e-7);
        Some(k.saturating_sub(1))
    }

    /// Presentation time (seconds) of frame `index`.
    pub fn frame_time(&self, index: usize) -> Option<f64> {
        self.times.get(index).copied()
    }

    /// Decode the frame shown at `secs` (full size).
    pub fn frame_at(&mut self, secs: f64) -> Result<Arc<Frame>> {
        let i = self.frame_index_at(secs).ok_or(VideoError::OutOfRange)?;
        self.frame(i)
    }

    /// True if frame `index` is in the cache (no decoding needed).
    pub fn is_cached(&self, index: usize) -> bool {
        self.cache.contains(index)
    }

    /// Decode frame `index` (presentation order), using the cache and decoding forward from the
    /// previous keyframe when needed.
    pub fn frame(&mut self, index: usize) -> Result<Arc<Frame>> {
        if index >= self.order.len() {
            return Err(VideoError::OutOfRange);
        }
        if let Some(f) = self.cache.get(index) {
            return Ok(f);
        }
        if let Codec::Unsupported(why) = &self.codec {
            return Err(VideoError::Unsupported(why.clone()));
        }
        let f = if self.codec.is_gop() {
            self.decode_gop(index)?
        } else {
            let s = self.order.get(index).copied().ok_or(VideoError::OutOfRange)?;
            Arc::new(self.decode_intra(s, index)?)
        };
        self.cache.put(index, Arc::clone(&f));
        Ok(f)
    }

    /// A small picture for the timeline at `secs`, at most `max_h` pixels high. For long-GOP
    /// codecs this is the keyframe at or before `secs` (cheap: one picture is decoded).
    pub fn thumbnail_at(&mut self, secs: f64, max_h: u32) -> Result<Arc<Frame>> {
        let i = self.frame_index_at(secs).ok_or(VideoError::OutOfRange)?;
        let max_h = max_h.clamp(4, 1080);
        let key = if self.codec.is_gop() {
            let s = self.order.get(i).copied().ok_or(VideoError::OutOfRange)?;
            let k = self.start_sample(s, i);
            self.rank.get(k).copied().unwrap_or(i)
        } else {
            i
        };
        if let Some(f) = self.thumbs.get((key, max_h)) {
            return Ok(f);
        }
        let full = if let Some(f) = self.cache.get(key) {
            f
        } else {
            if let Codec::Unsupported(why) = &self.codec {
                return Err(VideoError::Unsupported(why.clone()));
            }
            let s = self.order.get(key).copied().ok_or(VideoError::OutOfRange)?;
            if self.codec.is_gop() { Arc::new(self.decode_gop_single(s, i)?) } else { Arc::new(self.decode_intra(s, key)?) }
        };
        let w = (f64::from(max_h) * self.info.display_aspect()).round().clamp(1.0, 4096.0) as u32;
        let t = Arc::new(full.fit(w, max_h));
        self.thumbs.put((key, max_h), Arc::clone(&t));
        Ok(t)
    }

    /// `count` evenly spaced thumbnails across the movie (time, picture).
    pub fn thumbnails(&mut self, count: usize, max_h: u32) -> Vec<(f64, Result<Arc<Frame>>)> {
        let count = count.clamp(1, 1000);
        let d = self.info.duration;
        (0..count)
            .map(|k| {
                let t = d * k as f64 / count as f64;
                (t, self.thumbnail_at(t, max_h))
            })
            .collect()
    }

    fn read(&self, sample: usize) -> Result<Vec<u8>> {
        let t = self.mp4.tracks.get(self.track).ok_or(VideoError::NoVideoTrack)?;
        let size = t.samples.get(sample).map_or(0, |s| s.size);
        if size > 256 << 20 {
            return Err(VideoError::Decode("sample larger than 256 MiB".into()));
        }
        self.mp4.read_sample(&*self.src, self.track, sample).map_err(|e| VideoError::Container(e.to_string()))
    }

    fn decode_intra(&mut self, sample: usize, index: usize) -> Result<Frame> {
        let data = self.read(sample)?;
        let time = self.times.get(index).copied().unwrap_or(0.0);
        self.decoded += 1;
        match self.codec {
            Codec::ProRes => {
                let (hdr, ..) = crate::prores::FrameHeader::parse(&data).map_err(|e| VideoError::Decode(e.to_string()))?;
                let (w, h) = (u32::from(hdr.width), u32::from(hdr.height));
                if (w, h) != (self.info.width, self.info.height) {
                    return Err(VideoError::Decode(format!("{w}x{h} frame in a {}x{} track", self.info.width, self.info.height)));
                }
                let opts = crate::prores::DecodeOptions { bit_depth: Some(8), threads: false };
                let f = crate::prores::decode_frame_with(&data, &opts).map_err(|e| VideoError::Decode(e.to_string()))?;
                if u64::from(f.width) * u64::from(f.height) > MAX_PIXELS {
                    return Err(VideoError::Decode("picture too large".into()));
                }
                let (w, h) = (f.width as usize, f.height as usize);
                let cw = f.chroma_width() as usize;
                let narrow = |p: &[u16]| p.iter().map(|&v| v.min(255) as u8).collect::<Vec<u8>>();
                let (y, u, v) = (narrow(&f.y), narrow(&f.cb), narrow(&f.cr));
                let cx = if cw < w { 2 } else { 1 };
                let m = Matrix::for_code(f.color.matrix, f.height, false);
                let rgba = planar_to_rgba(w, h, &y, w, &u, &v, cw, cx, 1, &m);
                Ok(Frame { width: f.width, height: f.height, rgba, index, time })
            }
            Codec::Jpeg => {
                let img = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).map_err(|e| VideoError::Decode(e.to_string()))?;
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                if u64::from(width) * u64::from(height) > MAX_PIXELS {
                    return Err(VideoError::Decode("picture too large".into()));
                }
                Ok(Frame { width, height, rgba: rgba.into_raw(), index, time })
            }
            Codec::Dnx => {
                let p = adapt_dnx::decode(&data, &self.info)?;
                checked_frame(p.width, p.height, p.rgba, index, time)
            }
            _ => Err(VideoError::Unsupported("not an intra-frame codec".into())),
        }
    }

    /// A decoded long-GOP picture as a frame at presentation index `index`.
    fn yuv_frame(&self, yuv: &Yuv, index: usize) -> Result<Frame> {
        checked_frame(yuv.width, yuv.height, yuv.to_rgba(), index, self.times.get(index).copied().unwrap_or(0.0))
    }

    /// The sample (decode order) to start a decoder at to reach presentation index `index`, whose
    /// sample is `target`: the latest sync sample whose own presentation index is not after
    /// `index`. A sync sample can follow the target in presentation order (the leading pictures of
    /// an open-GOP random-access point are decoded after it but shown before it), and starting
    /// there would leave the target without its references; that sync sample is skipped. The
    /// search steps back over at most [`MAX_START_STEPS`] sync samples.
    fn start_sample(&self, target: usize, index: usize) -> usize {
        let Some(t) = self.mp4.tracks.get(self.track) else { return 0 };
        let mut s = t.sync_sample_before(target);
        for _ in 0..MAX_START_STEPS {
            if s == 0 || self.rank.get(s).is_none_or(|&r| r <= index) {
                break;
            }
            s = t.sync_sample_before(s - 1);
        }
        s
    }

    /// For codecs whose sync flag cannot be trusted: steps back from `start` while the decoder says
    /// the sample is not a real random-access point (at most [`MAX_START_STEPS`] times).
    fn confirm_start(&self, dec: &dyn GopDecoder, mut start: usize, index: usize) -> usize {
        if !dec.checks_random_access() {
            return start;
        }
        for _ in 0..MAX_START_STEPS {
            let ok = match self.read(start) {
                Ok(au) => dec.is_random_access(&au),
                // An unreadable sample fails the decode itself, with the real error.
                Err(_) => true,
            };
            if ok || start == 0 {
                break;
            }
            start = self.start_sample(start - 1, index);
        }
        start
    }

    /// Decode one sample with a fresh decoder (keyframe thumbnails). `index` is the presentation
    /// index asked for; the picture comes from the random-access sample at or before `sample`.
    fn decode_gop_single(&mut self, sample: usize, index: usize) -> Result<Frame> {
        let mut dec = self.codec.new_gop()?;
        let sample = self.confirm_start(&*dec, sample, index);
        let key = self.rank.get(sample).copied().unwrap_or(index);
        let data = self.read(sample)?;
        let mut pics = dec.decode(&data, key as i64)?;
        pics.extend(dec.flush());
        self.decoded += 1;
        let p = pics.iter().find(|p| p.pts == key as i64).or(pics.first()).ok_or_else(|| VideoError::Decode("no picture decoded".into()))?;
        self.yuv_frame(&p.yuv, key)
    }

    /// Decode presentation index `index` of a long-GOP codec: continue the running decoder when it
    /// is positioned before the picture, else start a new one at a random-access sample.
    fn decode_gop(&mut self, index: usize) -> Result<Arc<Frame>> {
        let target = self.order.get(index).copied().ok_or(VideoError::OutOfRange)?;
        let key = self.start_sample(target, index);
        let resume = self.gop.as_ref().is_some_and(|st| {
            st.ready.contains_key(&index) || (key >= st.start && key < st.next && st.next != usize::MAX && st.last_out.is_none_or(|lo| lo < index))
        });
        if !resume {
            let dec = self.codec.new_gop()?;
            let start = self.confirm_start(&*dec, key, index);
            self.gop = Some(GopState { dec, start, next: start, last_out: None, ready: BTreeMap::new() });
        }
        let total = self.order.len();
        let mut last_err: Option<String> = None;
        // Pictures this request has seen output with a presentation index past `index`.
        let mut past = 0usize;
        // Bounded: each iteration feeds one sample, or flushes once at the end.
        for _ in 0..=total.saturating_add(1) {
            let Some(st) = self.gop.as_mut() else { break };
            st.ready.retain(|q, _| *q >= index);
            if let Some(yuv) = st.ready.remove(&index) {
                return Ok(Arc::new(self.yuv_frame(&yuv, index)?));
            }
            // Decoders output in presentation order, so once output has gone well past `index`
            // without producing it the picture is not coming (a damaged sample, or a picture the
            // stream does not output). Stop here rather than decode the rest of the movie, and leave
            // the decoder where it is: the pictures after `index` that it already output wait in
            // `ready`, so the next frame still comes from this decoder. "Well past" is
            // [`PAST_PICTURES_BEFORE_GIVING_UP`] pictures, not one: output order can disagree with
            // the container's presentation order by the stream's reorder depth, and giving up at the
            // first picture past `index` failed frames that were about to arrive.
            if past >= PAST_PICTURES_BEFORE_GIVING_UP {
                break;
            }
            if st.next == usize::MAX {
                break;
            }
            let pics = if st.next < total {
                let s = st.next;
                st.next += 1;
                let pts = self.rank.get(s).copied().unwrap_or(0) as i64;
                let data = match self.mp4.read_sample(&*self.src, self.track, s) {
                    Ok(d) => d,
                    Err(e) => {
                        last_err = Some(e.to_string());
                        continue;
                    }
                };
                let Some(st) = self.gop.as_mut() else { break };
                match st.dec.decode(&data, pts) {
                    Ok(p) => p,
                    Err(VideoError::Decode(why)) => {
                        // Corrupt access unit: skip it (the decoder conceals what it can).
                        last_err = Some(why);
                        Vec::new()
                    }
                    Err(e) => {
                        // Valid but beyond this decoder (or a broken container): no sample will do better.
                        self.gop = None;
                        return Err(e);
                    }
                }
            } else {
                st.next = usize::MAX;
                st.dec.flush()
            };
            let Some(st) = self.gop.as_mut() else { break };
            for p in pics {
                let q = usize::try_from(p.pts).unwrap_or(usize::MAX);
                st.last_out = Some(st.last_out.map_or(q, |lo| lo.max(q)));
                self.decoded += 1;
                if q > index {
                    past += 1;
                }
                if q >= index {
                    st.ready.insert(q, p.yuv);
                }
            }
            // Pictures waiting for a later request: bounded by count and by bytes.
            trim_ready(&mut st.ready, READY_MAX_PICTURES, READY_MAX_BYTES);
        }
        Err(VideoError::Decode(last_err.unwrap_or_else(|| format!("frame {index} could not be decoded"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::Planes;

    fn frame(bytes: usize, index: usize) -> Arc<Frame> {
        Arc::new(Frame { width: 1, height: 1, rgba: vec![0; bytes], index, time: 0.0 })
    }

    fn yuv(bytes: usize, wide: bool) -> Yuv {
        let planes = if wide {
            Planes::U16 { y: vec![0; bytes / 2], u: Vec::new(), v: Vec::new() }
        } else {
            Planes::U8 { y: vec![0; bytes], u: Vec::new(), v: Vec::new() }
        };
        Yuv {
            width: 1,
            height: 1,
            planes,
            y_stride: 1,
            uv_stride: 1,
            cx: 2,
            cy: 2,
            bit_depth: if wide { 10 } else { 8 },
            matrix: 1,
            full_range: false,
        }
    }

    #[test]
    fn the_frame_cache_evicts_the_oldest_frames_by_bytes() {
        let mut c: Lru<usize> = Lru::new(100, 1000);
        for i in 0..5 {
            c.put(i, frame(400, i));
        }
        // 400-byte frames under a 1000-byte bound: only the two newest fit
        assert_eq!((c.items.len(), c.bytes), (2, 800));
        assert!(!c.contains(2) && c.contains(3) && c.contains(4));
        // a hit makes a frame the newest, so it outlives an older one
        assert!(c.get(3).is_some());
        c.put(5, frame(400, 5));
        assert!(c.contains(3) && c.contains(5) && !c.contains(4));
        // replacing a key swaps its bytes instead of adding them
        c.put(5, frame(100, 5));
        assert_eq!(c.bytes, 500);
        // the count cap still applies when the bytes are small
        let mut small: Lru<usize> = Lru::new(3, 1 << 30);
        for i in 0..10 {
            small.put(i, frame(10, i));
        }
        assert_eq!((small.items.len(), small.bytes), (3, 30));
    }

    #[test]
    fn a_picture_over_the_byte_bound_is_still_kept_alone() {
        let mut c: Lru<usize> = Lru::new(12, 100);
        c.put(0, frame(50, 0));
        c.put(1, frame(5000, 1));
        assert_eq!((c.items.len(), c.bytes), (1, 5000));
        assert!(c.contains(1));
        c.put(2, frame(60, 2));
        assert!(c.contains(2) && !c.contains(1) && c.bytes == 60, "the huge frame goes once a newer one arrives");
    }

    #[test]
    fn the_default_budget_is_one_gibibyte_and_holds_eight_k_frames() {
        assert_eq!(DEFAULT_CACHE_BYTES, 1 << 30);
        // RGBA frames the byte budget holds (the count ceiling is far above all of these)
        let held = |w: usize, h: usize| (DEFAULT_CACHE_BYTES / (w * h * 4)).min(DEFAULT_CACHE_FRAMES);
        assert_eq!(held(1920, 1080), 129);
        assert_eq!(held(3840, 2160), 32);
        assert_eq!(held(7680, 4320), 8);
        assert_eq!(held(8192, 8192), 4);
        assert_eq!(held(64, 64), DEFAULT_CACHE_FRAMES, "tiny pictures meet the count ceiling first");
    }

    #[test]
    fn ready_pictures_are_bounded_by_count_and_bytes_and_the_lowest_stays() {
        let mut r: BTreeMap<usize, Yuv> = (10..20).map(|i| (i, yuv(1000, false))).collect();
        trim_ready(&mut r, 32, 3500);
        assert_eq!(r.keys().copied().collect::<Vec<_>>(), [10, 11, 12], "the highest numbers go first");
        trim_ready(&mut r, 2, usize::MAX);
        assert_eq!(r.len(), 2);
        // 16-bit pictures count two bytes a sample
        let mut w: BTreeMap<usize, Yuv> = (0..4).map(|i| (i, yuv(1000, true))).collect();
        assert_eq!(w[&0].byte_len(), 1000);
        trim_ready(&mut w, 32, 2500);
        assert_eq!(w.len(), 2);
        // one picture over the bound is kept: it is the one being asked for
        let mut one: BTreeMap<usize, Yuv> = BTreeMap::from([(7, yuv(10_000, false)), (8, yuv(10_000, false))]);
        trim_ready(&mut one, 32, 5);
        assert_eq!(one.keys().copied().collect::<Vec<_>>(), [7]);
        let mut empty: BTreeMap<usize, Yuv> = BTreeMap::new();
        trim_ready(&mut empty, 0, 0);
        assert!(empty.is_empty());
    }
}

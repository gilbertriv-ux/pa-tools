//! A demo-like session for the interchange tests: the same shape as the application's demo (kick,
//! snare, hats, bass, a stereo pad, instrument and aux tracks, a master, markers), built from the
//! model alone with synthetic audio in the pool, plus a few edits that exercise the mapping:
//! fades, a crossfade, clip gain and a gain envelope.
#![allow(dead_code)]

use std::sync::Arc;

use soundcraft_audio_io::{AudioBuffer, FileFormat};
use soundcraft_model::{
    ChannelFormat, Clip, ClipId, Fade, FadeShape, MarkerKind, Session, Source, SourceAudio, SourceId, TrackId, TrackKind, VideoSource,
};
use soundcraft_time::{FrameRate, SampleRate};

pub const SR: u32 = 48_000;

/// A deterministic test tone: `channels` channels, sample `i` of channel `c` is a function of both.
pub fn tone(channels: usize, frames: usize, seed: u32) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            (0..frames)
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    0.5 * (t * (220.0 + 110.0 * (seed + c as u32) as f32) * std::f32::consts::TAU).sin() * (1.0 - 0.5 * (i % 4800) as f32 / 4800.0)
                })
                .collect()
        })
        .collect()
}

pub fn add_source(s: &mut Session, name: &str, channels: usize, frames: usize, seed: u32) -> SourceId {
    let id = SourceId(s.alloc());
    s.sources.push(Source {
        id,
        name: name.into(),
        path: format!("Audio Files/{name}.wav"),
        channels: channels as u16,
        frames: frames as u64,
        sample_rate: SR,
        format: FileFormat::Wav,
        time_reference: 0,
        unsaved: false,
    });
    let buf = AudioBuffer { sample_rate: SR, channels: tone(channels, frames, seed) };
    s.pool.insert(id, Arc::new(SourceAudio::new(buf)));
    id
}

pub fn place(s: &mut Session, track: TrackId, name: &str, source: SourceId, offset: i64, start: i64, len: i64) -> ClipId {
    let id = s.new_clip_id();
    let clip = Clip::audio(id, name, source, offset, start, len);
    if let Some(p) = s.track_mut(track).and_then(|t| t.playlist_mut()) {
        p.clips.push(clip);
        p.sort();
    }
    id
}

pub fn clip_mut(s: &mut Session, id: ClipId) -> &mut Clip {
    s.find_clip_mut(id).expect("clip")
}

/// The demo-like session. Returns it with the ids the tests refer to.
pub fn demo_like() -> Session {
    let mut s = Session::new("Midnight Groove", SampleRate::HZ_48000);
    s.frame_rate = FrameRate::Fps25;
    s.timecode_start = 3600 * i64::from(SR); // 01:00:00:00
    let total = 8 * SR as usize;
    let kick = add_source(&mut s, "Kick", 1, total, 1);
    let snare = add_source(&mut s, "Snare", 1, total, 2);
    let hats = add_source(&mut s, "Hats", 1, total, 3);
    let bass = add_source(&mut s, "Bass", 1, total, 4);
    let pad = add_source(&mut s, "Pad", 2, total, 5);
    let sr = i64::from(SR);

    let tk = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Kick"));
    let ts = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Snare"));
    let th = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Hats"));
    let tb = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Bass"));
    let tp = s.add_track(TrackKind::Audio, ChannelFormat::Stereo, Some("Pad"));

    // kick: three clips, the last with an S-curve fade-out like the demo's
    place(&mut s, tk, "Kick", kick, 0, sr, sr);
    place(&mut s, tk, "Kick", kick, sr, 3 * sr, sr);
    let k3 = place(&mut s, tk, "Kick", kick, 2 * sr, 5 * sr, 2 * sr);
    clip_mut(&mut s, k3).fade_out = Fade { len: sr / 2, shape: FadeShape::SCurve, curvature: 0.0 };

    // snare: a crossfade of 8000 samples between two clips
    let a = place(&mut s, ts, "Snare A", snare, 0, 0, sr);
    let b = place(&mut s, ts, "Snare B", snare, sr - 8_000, sr - 8_000, sr);
    clip_mut(&mut s, a).fade_out = Fade { len: 8_000, shape: FadeShape::EqualPower, curvature: 0.0 };
    clip_mut(&mut s, b).fade_in = Fade { len: 8_000, shape: FadeShape::EqualPower, curvature: 0.0 };

    // hats: a plain clip with a linear fade-in
    let h = place(&mut s, th, "Hats", hats, 4_800, 2 * sr, 4 * sr);
    clip_mut(&mut s, h).fade_in = Fade { len: 4_800, shape: FadeShape::Linear, curvature: 0.0 };

    // bass: fades on both edges and a static gain
    let bs = place(&mut s, tb, "Bass", bass, 0, 4 * sr, 3 * sr);
    let c = clip_mut(&mut s, bs);
    c.fade_in = Fade { len: 4_800, shape: FadeShape::EqualPower, curvature: 0.0 };
    c.fade_out = Fade { len: 9_600, shape: FadeShape::SCurve, curvature: 0.0 };
    c.gain_db = -4.5;

    // pad (stereo): an envelope
    let p = place(&mut s, tp, "Pad", pad, 0, 0, 6 * sr);
    clip_mut(&mut s, p).gain_env = vec![(0, -12.0), (sr / 2, 0.0), (5 * sr, -6.0)];

    // things the files cannot carry
    s.add_track(TrackKind::Instrument, ChannelFormat::Stereo, Some("Keys"));
    s.add_track(TrackKind::Aux, ChannelFormat::Stereo, Some("Verb"));
    s.add_track(TrackKind::Master, ChannelFormat::Stereo, Some("Master"));

    for (name, at) in [("Intro", 0), ("Verse", 2 * sr), ("Chorus", 4 * sr)] {
        s.add_marker(name, MarkerKind::Marker, at, at);
    }
    let m = s.add_marker("Fix this", MarkerKind::Selection, sr, 2 * sr);
    if let Some(loc) = s.markers.iter_mut().find(|x| x.id == m) {
        loc.comments = "noise on the downbeat".into();
        loc.color = Some([200, 40, 40]);
    }
    s
}

pub fn video_session() -> Session {
    let mut s = demo_like();
    let id = SourceId(s.alloc());
    s.videos.push(VideoSource {
        id,
        name: "Scene".into(),
        path: "/media/scene.mov".into(),
        width: 1280,
        height: 720,
        frame_rate: 25.0,
        duration: 6.0,
        codec: "h264".into(),
        offline: None,
    });
    let t = s.add_track(TrackKind::Video, ChannelFormat::Mono, Some("Picture"));
    let cid = s.new_clip_id();
    if let Some(p) = s.track_mut(t).and_then(|t| t.playlist_mut()) {
        p.clips.push(Clip::video(cid, "Scene", id, 24_960, 48_000, 5 * 48_000));
    }
    s
}

/// What the files can carry of one audio track's playlist, for comparing sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipView {
    pub start: i64,
    pub length: i64,
    pub offset: i64,
    pub gain_db: i32,
    pub fade_in: (i64, FadeShape),
    pub fade_out: (i64, FadeShape),
    pub env: Vec<(i64, i32)>,
    pub source: String,
}

pub fn views(s: &Session, kind: TrackKind) -> Vec<(String, Vec<ClipView>)> {
    s.tracks
        .iter()
        .filter(|t| t.kind == kind)
        .map(|t| {
            let clips = t
                .clips()
                .iter()
                .map(|c| ClipView {
                    start: c.start,
                    length: c.length,
                    offset: c.source_offset(),
                    gain_db: (c.gain_db * 100.0).round() as i32,
                    fade_in: (c.fade_in.len, c.fade_in.shape),
                    fade_out: (c.fade_out.len, c.fade_out.shape),
                    env: c.gain_env.iter().map(|&(t, v)| (t, (v * 100.0).round() as i32)).collect(),
                    source: c.source().and_then(|id| s.source(id)).map(|x| x.path.clone()).unwrap_or_default(),
                })
                .collect();
            (t.name.clone(), clips)
        })
        .collect()
}

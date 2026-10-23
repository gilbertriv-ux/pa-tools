//! A demo-like session through AAF and through OMF and back, and the edge cases of the mapping.

mod support;

use soundcraft_audio_io::decode;
use soundcraft_interchange::{ExportOptions, Format, ImportOptions, MediaMode, detect, export, import, merge_into};
use soundcraft_model::{ChannelFormat, Clip, ClipContent, Fade, FadeShape, MarkerKind, Session, TrackKind};
use soundcraft_time::{FrameRate, SampleRate};
use support::*;

fn opts() -> ImportOptions {
    ImportOptions { sample_rate: Some(SR), ..Default::default() }
}

fn round_trip(s: &Session, format: Format, eo: &ExportOptions) -> (Session, Vec<soundcraft_interchange::ExtractedMedia>) {
    let (bytes, _) = export(s, format, eo).expect("export");
    assert_eq!(detect(&bytes, None), Some(format));
    let (back, _) = import(&bytes, format, &opts()).expect("import");
    (back.session, back.extracted)
}

#[test]
fn a_demo_like_session_round_trips_through_aaf_and_omf() {
    let s = demo_like();
    for format in [Format::Aaf, Format::Omf] {
        let (back, extracted) = round_trip(&s, format, &ExportOptions::default());
        assert_eq!(views(&s, TrackKind::Audio), views(&back, TrackKind::Audio), "{format:?}");
        assert!(extracted.is_empty(), "linked media is not extracted");
        assert_eq!(back.sample_rate, s.sample_rate);
        assert_eq!(back.frame_rate, s.frame_rate);
        assert_eq!(back.timecode_start, s.timecode_start);
        assert_eq!(back.name, s.name);
        // the five audio files, once each
        assert_eq!(back.sources.len(), 5);
        let channels: Vec<(String, u16)> = back.sources.iter().map(|x| (x.name.clone(), x.channels)).collect();
        assert!(channels.contains(&("Pad".to_string(), 2)) && channels.contains(&("Kick".to_string(), 1)), "{channels:?}");
        // the stereo pad stays a stereo track
        let pad = back.tracks.iter().find(|t| t.name == "Pad").expect("Pad");
        assert_eq!(pad.format, ChannelFormat::Stereo);
    }
}

#[test]
fn what_cannot_be_carried_is_reported() {
    let s = demo_like();
    let (_, report) = export(&s, Format::Aaf, &ExportOptions::default()).expect("export");
    assert!(report.mentions("MIDI and instrument track"), "{report}");
    assert!(report.mentions("aux, master"), "{report}");
    assert!(report.has_warnings());
    let (_, report) = export(&s, Format::Omf, &ExportOptions::default()).expect("export");
    assert!(report.mentions("markers are not written to OMF"), "{report}");
}

#[test]
fn markers_round_trip_through_aaf() {
    let s = demo_like();
    let (back, _) = round_trip(&s, Format::Aaf, &ExportOptions::default());
    let view = |s: &Session| s.markers.iter().map(|m| (m.name.clone(), m.kind, m.start, m.end, m.comments.clone(), m.color)).collect::<Vec<_>>();
    assert_eq!(view(&s), view(&back));
    assert!(back.markers.iter().any(|m| m.kind == MarkerKind::Selection && m.color == Some([200, 40, 40])));
}

#[test]
fn every_frame_rate_and_the_start_timecode_round_trip() {
    for rate in FrameRate::ALL {
        for format in [Format::Aaf, Format::Omf] {
            let mut s = Session::new("rates", SampleRate::HZ_48000);
            s.frame_rate = rate;
            s.timecode_start = rate.samples_at_frame(3600 * rate.nominal() + 7, s.sample_rate);
            let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
            let src = add_source(&mut s, "x", 1, 48_000, 1);
            place(&mut s, t, "x", src, 0, 100, 10_000);
            let (back, _) = round_trip(&s, format, &ExportOptions::default());
            assert_eq!(back.frame_rate, rate, "{format:?} {rate:?}");
            assert_eq!(back.timecode_start, s.timecode_start, "{format:?} {rate:?}");
        }
    }
}

#[test]
fn odd_positions_are_sample_exact_at_every_sample_rate() {
    for hz in [8_000u32, 11_025, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000] {
        for format in [Format::Aaf, Format::Omf] {
            let mut s = Session::new("rates", SampleRate::new(hz).expect("rate"));
            let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
            let id = soundcraft_model::SourceId(s.alloc());
            s.sources.push(soundcraft_model::Source {
                id,
                name: "x".into(),
                path: "/m/x.wav".into(),
                channels: 1,
                frames: 5_000_000,
                sample_rate: hz,
                format: soundcraft_audio_io::FileFormat::Wav,
                time_reference: 0,
                unsaved: false,
            });
            place(&mut s, t, "a", id, 777, 12_345, 54_321);
            place(&mut s, t, "b", id, 1_000_003, 98_765, 4_321);
            let (bytes, _) = export(&s, format, &ExportOptions::default()).expect("export");
            let (back, _) = import(&bytes, format, &ImportOptions { sample_rate: Some(hz), ..Default::default() }).expect("import");
            assert_eq!(views(&s, TrackKind::Audio), views(&back.session, TrackKind::Audio), "{format:?} {hz}");
            assert_eq!(back.session.sample_rate.hz(), hz);
        }
    }
}

#[test]
fn embedded_audio_comes_back_as_wav_files_with_the_right_samples() {
    let s = demo_like();
    for format in [Format::Aaf, Format::Omf] {
        for bits in [16u16, 24] {
            let eo = ExportOptions { media: MediaMode::Embed, handles_ms: 100, bits, ..Default::default() };
            let (bytes, report) = export(&s, format, &eo).expect("export");
            assert!(!report.mentions("linked instead"), "{report}");
            let (back, _) = import(&bytes, format, &opts()).expect("import");
            assert_eq!(back.extracted.len(), 5, "{format:?}");
            // every imported clip plays the same samples as the original clip
            let tol = if bits == 16 { 1.0 / 16_000.0 } else { 1.0 / 4_000_000.0 };
            for (t_orig, t_back) in
                s.tracks.iter().filter(|t| t.kind == TrackKind::Audio).zip(back.session.tracks.iter().filter(|t| t.kind == TrackKind::Audio))
            {
                assert_eq!(t_orig.clips().len(), t_back.clips().len());
                for (co, cb) in t_orig.clips().iter().zip(t_back.clips()) {
                    assert_eq!((co.start, co.length), (cb.start, cb.length));
                    let (ClipContent::Audio { source: so, offset: oo }, ClipContent::Audio { source: sb, offset: ob }) = (&co.content, &cb.content)
                    else {
                        panic!("audio")
                    };
                    let orig = s.pool.get(*so).expect("pool");
                    let file = back.extracted.iter().find(|e| e.source == *sb).expect("extracted");
                    let (info, buf) = decode(&file.wav, Some("wav")).expect("a valid WAV file");
                    assert_eq!(info.sample_rate, SR);
                    assert_eq!(buf.num_channels(), orig.buffer.num_channels());
                    for ch in 0..buf.num_channels() {
                        for probe in [0, 1, co.length / 2, co.length - 1] {
                            let a = orig.buffer.channels[ch][(oo + probe) as usize];
                            let b = buf.channels[ch][(ob + probe) as usize];
                            assert!((a - b).abs() <= tol, "{format:?} {bits} bit {} ch{ch}+{probe}: {a} vs {b}", co.name);
                        }
                    }
                }
            }
            // the file is trimmed to the used range plus the handles, not the whole 8 seconds
            let kick = back.session.sources.iter().find(|x| x.name == "Kick").expect("Kick");
            assert!(kick.frames < 5 * u64::from(SR), "{}", kick.frames);
            assert!(back.extracted.iter().all(|e| e.path.starts_with("Audio Files/") && e.path.ends_with(".wav")));
        }
    }
}

#[test]
fn breakout_to_mono_embeds_one_file_per_channel() {
    let s = demo_like();
    let eo = ExportOptions { media: MediaMode::Embed, breakout_to_mono: true, ..Default::default() };
    for format in [Format::Aaf, Format::Omf] {
        let (back, extracted) = round_trip(&s, format, &eo);
        let names: Vec<&str> = back.tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Pad.L") && names.contains(&"Pad.R"), "{names:?}");
        assert!(back.tracks.iter().filter(|t| t.kind == TrackKind::Audio).all(|t| t.format == ChannelFormat::Mono));
        // six files: four mono sources and the pad's two channels
        assert_eq!(extracted.len(), 6, "{format:?}");
        let pad_l = extracted.iter().find(|e| e.path.contains("Pad")).expect("a Pad file");
        let (_, buf) = decode(&pad_l.wav, Some("wav")).expect("wav");
        assert_eq!(buf.num_channels(), 1);
    }
}

#[test]
fn linked_breakout_warns_that_channels_cannot_be_selected_on_a_clip() {
    let s = demo_like();
    let (bytes, _) = export(&s, Format::Aaf, &ExportOptions { breakout_to_mono: true, ..Default::default() }).expect("export");
    let (_, report) = import(&bytes, Format::Aaf, &opts()).expect("import");
    assert!(report.mentions("channel"), "{report}");
}

#[test]
fn video_tracks_round_trip_through_aaf_and_are_left_out_of_omf() {
    let s = video_session();
    let (bytes, _) = export(&s, Format::Aaf, &ExportOptions::default()).expect("export");
    let (back, _) = import(&bytes, Format::Aaf, &opts()).expect("import");
    let video = |s: &Session| {
        s.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| {
                t.clips().iter().map(|c| {
                    (c.start, c.length, c.source_offset(), c.video_source().and_then(|id| s.video(id)).map(|v| (v.path.clone(), v.width, v.height)))
                })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(video(&s), video(&back.session));
    assert_eq!(back.session.videos.len(), 1);
    assert!((back.session.videos[0].frame_rate - 25.0).abs() < 1e-9);
    assert!((back.session.videos[0].duration - 6.0).abs() < 1e-6);
    let (bytes, report) = export(&s, Format::Omf, &ExportOptions::default()).expect("export");
    assert!(report.mentions("video track"), "{report}");
    let (back, _) = import(&bytes, Format::Omf, &opts()).expect("import");
    assert!(back.session.videos.is_empty());
    let no_video = ExportOptions { include_video: false, ..Default::default() };
    let (bytes, report) = export(&s, Format::Aaf, &no_video).expect("export");
    assert!(report.mentions("video track"));
    assert!(import(&bytes, Format::Aaf, &opts()).expect("import").0.session.videos.is_empty());
}

#[test]
fn muted_clips_become_gaps_and_stretched_clips_are_reported() {
    let mut s = Session::new("m", SampleRate::HZ_48000);
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
    let src = add_source(&mut s, "x", 1, 96_000, 1);
    let a = place(&mut s, t, "a", src, 0, 0, 10_000);
    let b = place(&mut s, t, "b", src, 10_000, 10_000, 10_000);
    clip_mut(&mut s, a).muted = true;
    clip_mut(&mut s, b).stretch = 1.5;
    let (bytes, report) = export(&s, Format::Aaf, &ExportOptions::default()).expect("export");
    assert!(report.mentions("muted clip"), "{report}");
    assert!(report.mentions("time stretch"), "{report}");
    let (back, _) = import(&bytes, Format::Aaf, &opts()).expect("import");
    let v = views(&back.session, TrackKind::Audio);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].1.iter().map(|c| (c.start, c.length)).collect::<Vec<_>>(), vec![(10_000, 10_000)]);
}

#[test]
fn clips_that_overlap_other_than_as_a_crossfade_go_to_extra_tracks() {
    let mut s = Session::new("o", SampleRate::HZ_48000);
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Dlg"));
    let src = add_source(&mut s, "x", 1, 200_000, 1);
    place(&mut s, t, "a", src, 0, 0, 50_000);
    place(&mut s, t, "inside", src, 100, 10_000, 5_000); // inside a
    place(&mut s, t, "c", src, 60_000, 40_000, 30_000); // overlaps a's end: a crossfade
    place(&mut s, t, "same", src, 0, 40_000, 30_000); // starts with c
    let (bytes, _) = export(&s, Format::Aaf, &ExportOptions::default()).expect("export");
    let (back, _) = import(&bytes, Format::Aaf, &opts()).expect("import");
    let v = views(&back.session, TrackKind::Audio);
    assert!(v.len() >= 2, "{v:?}");
    let all: usize = v.iter().map(|x| x.1.len()).sum();
    assert_eq!(all, 4);
    // every clip still plays at its place, with its material
    let mut got: Vec<(i64, i64, i64)> = v.iter().flat_map(|x| x.1.iter().map(|c| (c.start, c.length, c.offset))).collect();
    got.sort_unstable();
    let mut want: Vec<(i64, i64, i64)> = views(&s, TrackKind::Audio).iter().flat_map(|x| x.1.iter().map(|c| (c.start, c.length, c.offset))).collect();
    want.sort_unstable();
    assert_eq!(got, want);
}

#[test]
fn fades_longer_than_the_clip_and_clips_before_the_start_are_clamped() {
    let mut s = Session::new("f", SampleRate::HZ_48000);
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
    let src = add_source(&mut s, "x", 1, 96_000, 1);
    let a = place(&mut s, t, "a", src, 5_000, 1_000, 2_000);
    clip_mut(&mut s, a).fade_in = Fade { len: 1_900, shape: FadeShape::Linear, curvature: 0.0 };
    clip_mut(&mut s, a).fade_out = Fade { len: 99_999, shape: FadeShape::Linear, curvature: 0.0 };
    place(&mut s, t, "early", src, 20_000, -3_000, 10_000); // starts 3000 samples before zero
    for format in [Format::Aaf, Format::Omf] {
        let (bytes, _) = export(&s, format, &ExportOptions::default()).expect("export");
        let (back, _) = import(&bytes, format, &opts()).expect("import");
        let v = views(&back.session, TrackKind::Audio);
        let clips: Vec<_> = v.iter().flat_map(|x| x.1.iter()).collect();
        let early = clips.iter().find(|c| c.start == 0).expect("early clip");
        assert_eq!((early.length, early.offset), (7_000, 23_000), "cut at zero, material kept in place");
        let a = clips.iter().find(|c| c.start == 1_000).expect("a");
        assert!(a.fade_in.0 + a.fade_out.0 <= a.length, "{a:?}");
    }
}

#[test]
fn exports_are_reproducible() {
    let s = demo_like();
    for format in [Format::Aaf, Format::Omf] {
        let a = export(&s, format, &ExportOptions::default()).expect("export").0;
        let b = export(&s, format, &ExportOptions::default()).expect("export").0;
        assert_eq!(a, b, "{format:?}");
    }
    let small = export(&s, Format::Aaf, &ExportOptions { small_sectors: true, ..Default::default() }).expect("export").0;
    let (back, _) = import(&small, Format::Aaf, &opts()).expect("import");
    assert_eq!(views(&s, TrackKind::Audio), views(&back.session, TrackKind::Audio));
}

#[test]
fn an_import_merges_into_an_open_session_with_fresh_ids() {
    let s = demo_like();
    let (bytes, _) = export(&s, Format::Aaf, &ExportOptions { media: MediaMode::Embed, ..Default::default() }).expect("export");
    let (imported, _) = import(&bytes, Format::Aaf, &opts()).expect("import");
    let mut target = Session::new("target", SampleRate::HZ_48000);
    let existing = add_source(&mut target, "Existing", 1, 1000, 9);
    let t0 = target.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Pad"));
    place(&mut target, t0, "e", existing, 0, 0, 100);
    target.add_track(TrackKind::Master, ChannelFormat::Stereo, Some("Master"));
    let n_extracted = imported.extracted.len();
    let merged = merge_into(&mut target, imported, 10_000).expect("merge");
    assert_eq!(merged.tracks.len(), 5);
    assert_eq!(merged.extracted.len(), n_extracted);
    assert_eq!(target.sources.len(), 6);
    // ids are unique across the session
    let mut ids: Vec<u64> =
        target.tracks.iter().map(|t| t.id.0).chain(target.sources.iter().map(|x| x.id.0)).chain(target.all_clips().map(|c| c.id.0)).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n);
    // the imported pad track was renamed, shifted, and points at an imported source
    let pad = target.tracks.iter().find(|t| t.name == "Pad 1").expect("renamed Pad");
    assert_eq!(pad.clips()[0].start, 10_000);
    let src = pad.clips()[0].source().expect("audio");
    assert!(merged.sources.contains(&src) && target.source(src).is_some());
    assert!(merged.extracted.iter().all(|e| target.source(e.source).is_some()));
    // the master stays last
    assert_eq!(target.tracks.last().map(|t| t.kind), Some(TrackKind::Master));
    // markers were added, shifted
    assert!(target.markers.iter().any(|m| m.name == "Verse" && m.start == 2 * 48_000 + 10_000));
    // a fragment at another sample rate is refused
    let (frag, _) = import(&bytes, Format::Aaf, &ImportOptions { sample_rate: Some(44_100), ..Default::default() }).expect("import");
    assert!(merge_into(&mut target, frag, 0).is_err());
}

#[test]
fn clip_gain_and_envelopes_survive() {
    let s = demo_like();
    let (back, _) = round_trip(&s, Format::Omf, &ExportOptions::default());
    let bass = back.tracks.iter().find(|t| t.name == "Bass").expect("Bass").clips()[0].gain_db;
    assert!((bass - -4.5).abs() < 0.01, "{bass}");
    let pad = back.tracks.iter().find(|t| t.name == "Pad").expect("Pad").clips()[0].gain_env.clone();
    assert_eq!(pad.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, 24_000, 240_000]);
    assert!((pad[0].1 - -12.0).abs() < 0.01 && pad[1].1.abs() < 0.01 && (pad[2].1 - -6.0).abs() < 0.01, "{pad:?}");
    // a clip with static gain and an envelope: the envelope is relative to the static gain
    let mut s = Session::new("g", SampleRate::HZ_48000);
    let t = s.add_track(TrackKind::Audio, ChannelFormat::Mono, None);
    let src = add_source(&mut s, "x", 1, 96_000, 1);
    let c = place(&mut s, t, "c", src, 0, 0, 48_000);
    clip_mut(&mut s, c).gain_db = -3.0;
    clip_mut(&mut s, c).gain_env = vec![(0, 0.0), (24_000, -6.0)];
    let (back, _) = round_trip(&s, Format::Aaf, &ExportOptions::default());
    let env = &back.tracks[0].clips()[0].gain_env;
    assert!((env[0].1 - -3.0).abs() < 0.01 && (env[1].1 - -9.0).abs() < 0.01, "{env:?}");
}

#[test]
fn the_clip_is_a_clip_of_the_model() {
    // keeps the helper honest: a built clip is what the views describe
    let c = Clip::audio(soundcraft_model::ClipId(1), "x", soundcraft_model::SourceId(2), 5, 6, 7);
    assert_eq!((c.start, c.length, c.source_offset()), (6, 7, 5));
}

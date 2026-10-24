//! Disk-stream control regressions. No audio hardware or file cleanup is required.
use soundcraft_audio_io::AudioBuffer;
use soundcraft_model::{ChannelFormat, Clip, Session, SourceAudio, SourceId, TrackKind};
use soundcraft_time::Range;
use std::sync::Arc;

#[test]
fn resident_sources_keep_exact_mixer_output_through_the_storage_abstraction() {
    let mut session = Session::default();
    let frames = 8192;
    let source = SourceId(900);
    let data = (0..frames).map(|i| (i as f32 * 0.01).sin() * 0.25).collect::<Vec<_>>();
    session.pool.insert(source, Arc::new(SourceAudio::new(AudioBuffer { sample_rate: session.sample_rate.hz(), channels: vec![data.clone()] })));
    let track = session.add_track(TrackKind::Audio, ChannelFormat::Mono, Some("Reference"));
    let id = session.new_clip_id();
    session.track_mut(track).unwrap().playlist_mut().unwrap().clips.push(Clip::audio(id, "Reference", source, 0, 0, frames as i64));
    let rendered = soundcraft_mix::render_clips(&session, track, Range::new(0, frames as i64));
    assert_eq!(rendered, vec![data]);
    soundcraft_mix::source_stream::prepare(&session, 0, None, || true).unwrap();
    assert_eq!(soundcraft_mix::source_stream::first_error(&session), None);
    assert_eq!(soundcraft_mix::source_stream::underruns(&session), 0);
}

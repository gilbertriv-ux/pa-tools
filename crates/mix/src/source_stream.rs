//! Source read-ahead and disk-stream diagnostics shared by playback and offline export.
//!
//! `prefetch` only posts requests and is safe on the audio thread. Error strings are collected
//! only by UI/control/export threads: a failed disk read must not be silently exported as silence.
use soundcraft_model::{Clip, ClipContent, Session};
use soundcraft_time::{Range, Samples};

/// Queue the block containing the playhead plus the next source block, and the loop head.
/// Requests carry source positions, not decoder cursor positions; a seek never consumes stale
/// data from an earlier position. This performs no file access, waits, or allocation.
pub fn prefetch(session: &Session, position: Samples, looped: Option<Range>) {
    for track in &session.tracks {
        if track.inactive {
            continue;
        }
        for clip in track.clips().iter().filter(|c| !c.muted) {
            if let Some(range) = looped {
                prefetch_clip(session, clip, range.start);
            }
            // Current-position requests have priority over optional loop-head lookahead.
            prefetch_clip(session, clip, position);
        }
    }
}

fn prefetch_clip(session: &Session, clip: &Clip, position: Samples) {
    let ClipContent::Audio { source, offset } = clip.content else { return };
    if position < clip.start || position >= clip.start.saturating_add(clip.length) {
        return;
    }
    let Some(audio) = session.pool.get(source) else { return };
    let stretch = if clip.stretch.is_finite() && clip.stretch > 0.0 { clip.stretch } else { 1.0 };
    let relative = position.saturating_sub(clip.start);
    let at = offset as f64 + relative as f64 / stretch;
    if at.is_finite() && at >= 0.0 && at < audio.frames() as f64 {
        let frame = at as usize;
        audio.prefetch(frame.saturating_add(4096));
        audio.prefetch(frame);
    }
}

/// First source error for reporting or aborting an offline export. Call off the audio thread.
/// A source read failure is distinct from an out-of-range sample (ordinary timeline silence).
pub fn first_error(session: &Session) -> Option<String> {
    session.pool.ids().find_map(|id| {
        let source = session.pool.get(id)?;
        source.stream_error().map(|error| format!("audio source {}: {error}", id.0))
    })
}

/// Cumulative source-cache misses; read without collecting or formatting error strings.
/// The reader defines whether a miss counts per sample or per requested block.
pub fn underruns(session: &Session) -> u64 {
    session.pool.ids().filter_map(|id| session.pool.get(id)).fold(0u64, |count, source| count.saturating_add(source.underruns()))
}

/// Warm the initial disk pages on a control worker before playback starts. Returns false when
/// the control generation changed. No caller may invoke this from the audio callback.
pub fn prepare(session: &Session, position: Samples, looped: Option<Range>, current: impl Fn() -> bool) -> Result<bool, String> {
    for track in &session.tracks {
        if !current() {
            return Ok(false);
        }
        if track.inactive {
            continue;
        }
        for clip in track.clips().iter().filter(|clip| !clip.muted) {
            let ClipContent::Audio { source, offset } = clip.content else {
                continue;
            };
            let Some(audio) = session.pool.get(source) else {
                continue;
            };
            if !audio.is_file_backed() {
                continue;
            }
            let stretch = if clip.stretch.is_finite() && clip.stretch > 0.0 { clip.stretch } else { 1.0 };
            for at in looped.map(|range| range.start).into_iter().chain(std::iter::once(position)) {
                if !current() {
                    return Ok(false);
                }
                if at < clip.start || at >= clip.start.saturating_add(clip.length) {
                    continue;
                }
                let frame = offset as f64 + at.saturating_sub(clip.start) as f64 / stretch;
                if !frame.is_finite() || frame < 0.0 || frame >= audio.frames() as f64 {
                    continue;
                }
                for frame in [frame as usize, (frame as usize).saturating_add(4096)] {
                    if !current() {
                        return Ok(false);
                    }
                    if frame < audio.frames() && audio.sample(0, frame, false).is_none() {
                        return Err(audio.stream_error().unwrap_or_else(|| "cannot prepare audio source block".into()));
                    }
                }
            }
        }
        if let Some(source) = track.instrument.as_ref().and_then(|insert| insert.sample)
            && let Some(audio) = session.pool.get(source)
            && audio.is_file_backed()
            && audio.frames() > 0
            && audio.sample(0, 0, false).is_none()
        {
            return Err(audio.stream_error().unwrap_or_else(|| "cannot prepare instrument sample".into()));
        }
    }
    Ok(current())
}

use soundcraft_dsp::stream_resample::{MAX_INPUT_FRAMES, ResampleError, StreamError, StreamResampler};
use std::convert::Infallible;

fn convert(input: &[Vec<f32>], from: u32, to: u32, chunk: usize) -> Vec<Vec<f32>> {
    let mut s = StreamResampler::new(from, to, input.len()).unwrap();
    let mut out = vec![Vec::new(); input.len()];
    let len = input.first().map_or(0, Vec::len);
    for start in (0..len).step_by(chunk) {
        let end = (start + chunk).min(len);
        let block: Vec<Vec<f32>> = input.iter().map(|c| c[start..end].to_vec()).collect();
        s.try_process(&block, end == len, |frame| {
            for (dst, sample) in out.iter_mut().zip(frame) {
                dst.push(*sample);
            }
            Ok::<_, Infallible>(())
        })
        .unwrap();
    }
    out
}

#[test]
fn arbitrary_chunk_boundaries_have_identical_output_and_exact_duration() {
    let input = vec![(0..9_973).map(|i| (i as f32 * 0.013).sin()).collect(), (0..9_973).map(|i| (i as f32 * 0.017).cos()).collect()];
    for (from, to) in [(44_100, 48_000), (96_000, 44_100), (48_000, 48_000), (8_000, 192_000)] {
        let expected = convert(&input, from, to, input[0].len());
        assert_eq!(expected[0].len(), (input[0].len() as u128 * u128::from(to)).div_ceil(u128::from(from)) as usize);
        for chunk in [1, 2, 7, 127, 4096] {
            assert_eq!(convert(&input, from, to, chunk), expected);
        }
    }
}

#[test]
fn equal_rates_pass_samples_through_and_interpolation_crosses_blocks() {
    assert_eq!(convert(&[vec![0.0, 1.0, -0.5, 1e30, 0.1]], 48_000, 48_000, 1), [vec![0.0, 1.0, -0.5, 1e30, 0.1]]);
    assert_eq!(convert(&[vec![0.0, 1.0, 2.0]], 24_000, 48_000, 1), [vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.0]]);
}

#[test]
fn retained_working_set_does_not_grow_with_duration() {
    let mut s = StreamResampler::new(44_100, 48_000, 2).unwrap();
    let bytes = s.retained_sample_bytes();
    let block = vec![vec![0.25; 1024]; 2];
    let mut count = 0u64;
    for _ in 0..500 {
        s.try_process(&block, false, |_| {
            count += 1;
            Ok::<_, Infallible>(())
        })
        .unwrap();
        assert_eq!(s.retained_sample_bytes(), bytes);
    }
    s.try_process(&[vec![], vec![]], true, |_| {
        count += 1;
        Ok::<_, Infallible>(())
    })
    .unwrap();
    assert_eq!(count, (500u64 * 1024 * 48_000).div_ceil(44_100));
    assert_eq!(bytes, 2 * 2 * std::mem::size_of::<f32>());
}

#[test]
fn validates_before_consumption_and_propagates_sink_failure() {
    assert!(StreamResampler::new(0, 48_000, 2).is_err());
    assert!(StreamResampler::new(48_000, 48_000, 0).is_err());
    let mut s = StreamResampler::new(48_000, 48_000, 2).unwrap();
    assert!(matches!(
        s.try_process(&[vec![0.0], vec![]], false, |_| Ok::<_, Infallible>(())),
        Err(StreamError::Resample(ResampleError::InvalidBlock))
    ));
    assert_eq!(s.input_frames(), 0);
    assert!(s.try_process(&vec![vec![0.0; MAX_INPUT_FRAMES + 1]; 2], false, |_| Ok::<_, Infallible>(())).is_err());
    assert!(matches!(s.try_process(&[vec![0.0], vec![0.0]], false, |_| Err("disk full")), Err(StreamError::Output("disk full"))));
    assert!(matches!(s.try_process(&[vec![], vec![]], true, |_| Ok::<_, Infallible>(())), Err(StreamError::Resample(ResampleError::Finished))));
}

fn windowed(input: &[Vec<f32>], from: u32, to: u32) -> Vec<Vec<f32>> {
    let mut output = vec![Vec::new(); input.len()];
    let frames = input.first().map_or(0, Vec::len);
    let count = soundcraft_dsp::stream_resample::resample_windowed(
        from,
        to,
        input.len(),
        frames as u64,
        |channel, frame| Ok::<_, Infallible>(input[channel][frame as usize]),
        |frame| {
            for (channel, sample) in output.iter_mut().zip(frame) {
                channel.push(*sample);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count as usize, output[0].len());
    output
}

#[test]
fn windowed_output_matches_existing_offline_kernel_and_boundary_behavior() {
    let signal =
        vec![(0..1_537).map(|i| (i as f32 * 0.017).sin()).collect(), (0..1_537).map(|i| if i == 0 || i == 1_536 { 1.0 } else { 0.0 }).collect()];
    for (from, to) in [(44_100, 48_000), (96_000, 44_100), (48_000, 48_000), (48_000, 8_000)] {
        let reference = soundcraft_dsp::offline::resample(&signal, from, to);
        let output = windowed(&signal, from, to);
        assert_eq!(output[0].len(), reference[0].len());
        for (actual, expected) in output.iter().flatten().zip(reference.iter().flatten()) {
            assert!((actual - expected).abs() < 3e-6, "{from}->{to}: {actual} versus {expected}");
        }
    }
}

#[test]
fn windowed_downsampling_rejects_out_of_band_energy() {
    let input = |hz: f64| vec![(0..8_192).map(|i| (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin() as f32).collect::<Vec<_>>()];
    let rms = |signal: &[f32]| {
        let core = &signal[128..signal.len() - 128];
        (core.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / core.len() as f64).sqrt()
    };
    let wanted = windowed(&input(1_000.0), 48_000, 16_000);
    let aliased = windowed(&input(12_000.0), 48_000, 16_000);
    assert!(rms(&wanted[0]) > 0.65);
    assert!(rms(&aliased[0]) < 0.001);
}

#[test]
fn windowed_preserves_input_errors_and_sink_errors_without_retry() {
    use soundcraft_dsp::stream_resample::{WindowedError, resample_windowed};
    let input_error = resample_windowed(48_000, 44_100, 1, 100, |_, _| Err("source truncated"), |_| Ok(()));
    assert!(matches!(input_error, Err(WindowedError::Input("source truncated"))));
    let output_error = resample_windowed(48_000, 44_100, 1, 100, |_, _| Ok(0.0), |_| Err("disk full"));
    assert!(matches!(output_error, Err(WindowedError::Output("disk full"))));
    let empty = resample_windowed(48_000, 44_100, 1, 0, |_, _| Err::<f32, _>("must not read"), |_| Err("must not write"));
    assert_eq!(empty.unwrap(), 0);
}

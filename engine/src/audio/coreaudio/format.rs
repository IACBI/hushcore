//! Buffer-size, sample-format and channel policy for the Core Audio streams. Pure functions, so the unit
//! tests run on macOS CI without an audio device.

use cpal::{BufferSize, Error as CpalError, ErrorKind, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

/// Callback size requested from the device. Clamped into the range the device reports.
pub const PREFERRED_FRAMES: u32 = 128;

/// `BufferSize::Fixed` with `preferred` clamped into the device's range, or `Default` when the device
/// reports no usable range.
pub fn buffer_size(range: &SupportedBufferSize, preferred: u32) -> BufferSize {
    match *range {
        SupportedBufferSize::Range { min, max } if max > 0 => {
            let low = min.min(max);
            BufferSize::Fixed(preferred.clamp(low, max))
        }
        _ => BufferSize::Default,
    }
}

/// Duration of `frames` at `rate`, in milliseconds.
pub fn period_ms(frames: u32, rate: u32) -> f32 {
    frames as f32 * 1000.0 / rate as f32
}

/// True when some 32-bit float configuration of the device covers `rate`. cpal reports every macOS stream
/// as F32 (device.rs), so this is the sample-format check; a device that offers only other formats fails it.
pub fn supports(configs: &[SupportedStreamConfigRange], rate: u32) -> bool {
    configs.iter().any(|c| c.sample_format() == SampleFormat::F32 && c.contains_rate(rate))
}

/// Averages the `channels` interleaved samples of each frame into mono. `dst` receives frames
/// `offset..offset + dst.len()` of `src`; frames past the end of `src` become silence.
pub fn downmix(src: &[f32], channels: usize, offset: usize, dst: &mut [f32]) {
    let gain = 1.0 / channels as f32;
    let mut written = 0;
    for (out, frame) in dst.iter_mut().zip(src.chunks_exact(channels).skip(offset)) {
        *out = frame.iter().sum::<f32>() * gain;
        written += 1;
    }
    dst[written..].fill(0.0);
}

/// Writes each mono sample to every channel of its frame. `dst` holds `mono.len()` interleaved frames.
pub fn upmix(mono: &[f32], channels: usize, dst: &mut [f32]) {
    for (frame, &sample) in dst.chunks_exact_mut(channels).zip(mono) {
        frame.fill(sample);
    }
}

/// True only when cpal reports `PermissionDenied`, which cpal maps from an `Unauthorized` AudioUnit status.
/// Messages are not inspected: a silent or muted input must not be reported as a refused microphone.
pub fn is_permission_error(err: &CpalError) -> bool {
    err.kind() == ErrorKind::PermissionDenied
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{ChannelCount, SampleRate};

    fn range(min: u32, max: u32) -> SupportedBufferSize {
        SupportedBufferSize::Range { min, max }
    }

    fn config(min_rate: SampleRate, max_rate: SampleRate, format: SampleFormat) -> SupportedStreamConfigRange {
        let channels: ChannelCount = 2;
        SupportedStreamConfigRange::new(channels, min_rate, max_rate, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn preferred_size_is_clamped_into_the_device_range() {
        assert_eq!(buffer_size(&range(16, 4096), 128), BufferSize::Fixed(128));
        assert_eq!(buffer_size(&range(256, 1024), 128), BufferSize::Fixed(256));
        assert_eq!(buffer_size(&range(1, 64), 128), BufferSize::Fixed(64));
        assert_eq!(buffer_size(&range(64, 64), 128), BufferSize::Fixed(64));
    }

    #[test]
    fn unusable_ranges_fall_back_to_default() {
        assert_eq!(buffer_size(&SupportedBufferSize::Unknown, 128), BufferSize::Default);
        assert_eq!(buffer_size(&range(0, 0), 128), BufferSize::Default);
    }

    #[test]
    fn period_follows_frames_and_rate() {
        assert_eq!(period_ms(480, 48_000), 10.0);
    }

    #[test]
    fn downmix_averages_each_frame() {
        let src = [1.0f32, 0.0, -0.5, 0.5];
        let mut dst = [0f32; 2];
        downmix(&src, 2, 0, &mut dst);
        assert_eq!(dst, [0.5, 0.0]);
    }

    #[test]
    fn downmix_honours_the_offset_and_silences_the_tail() {
        let src = [1.0f32, 1.0, 2.0, 2.0];
        let mut dst = [9f32; 3];
        downmix(&src, 2, 1, &mut dst);
        assert_eq!(dst, [2.0, 0.0, 0.0]);
    }

    #[test]
    fn upmix_duplicates_into_every_channel() {
        let mut dst = [0f32; 6];
        upmix(&[0.25, -1.0], 3, &mut dst);
        assert_eq!(dst, [0.25, 0.25, 0.25, -1.0, -1.0, -1.0]);
    }

    #[test]
    fn rate_support_needs_f32_and_a_covering_range() {
        let f32_44_48 = config(44_100, 48_000, SampleFormat::F32);
        assert!(supports(&[f32_44_48], 48_000));
        assert!(!supports(&[f32_44_48], 96_000));
        assert!(!supports(&[config(44_100, 48_000, SampleFormat::I16)], 48_000));
        assert!(!supports(&[], 48_000));
    }

    #[test]
    fn only_permission_denied_counts_as_a_refused_microphone() {
        assert!(is_permission_error(&CpalError::new(ErrorKind::PermissionDenied)));
        assert!(!is_permission_error(&CpalError::with_message(ErrorKind::BackendError, "Unauthorized")));
        assert!(!is_permission_error(&CpalError::with_message(ErrorKind::BackendError, "no device")));
    }
}

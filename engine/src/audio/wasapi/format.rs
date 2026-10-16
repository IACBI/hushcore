//! Sample formats the WASAPI streams can use, and conversion to and from the engine's mono f32.

use anyhow::{bail, Result};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
// Data1 of KSDATAFORMAT_SUBTYPE_PCM / KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
const SUBTYPE_PCM: u32 = 1;
const SUBTYPE_IEEE_FLOAT: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    I16,
    I24,
    I32,
}

impl SampleKind {
    pub fn bytes(self) -> usize {
        match self {
            SampleKind::F32 | SampleKind::I32 => 4,
            SampleKind::I24 => 3,
            SampleKind::I16 => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub kind: SampleKind,
}

impl StreamFormat {
    pub fn frame_bytes(&self) -> usize {
        self.channels as usize * self.kind.bytes()
    }

    /// A plain (non-extensible) WAVEFORMATEX for this format, used to request exclusive or converted streams.
    pub fn to_waveformatex(self) -> WAVEFORMATEX {
        let bytes = self.kind.bytes() as u16;
        let block_align = self.channels * bytes;
        WAVEFORMATEX {
            wFormatTag: if self.kind == SampleKind::F32 { WAVE_FORMAT_IEEE_FLOAT } else { WAVE_FORMAT_PCM },
            nChannels: self.channels,
            nSamplesPerSec: self.sample_rate,
            nAvgBytesPerSec: self.sample_rate * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: bytes * 8,
            cbSize: 0,
        }
    }

    /// Reads a WAVEFORMATEX (possibly extensible) that the driver handed us.
    ///
    /// # Safety
    /// `ptr` must point at a readable WAVEFORMATEX, followed by the extension when its tag is extensible.
    pub unsafe fn from_waveformatex(ptr: *const WAVEFORMATEX) -> Result<Self> {
        // WAVEFORMATEX is packed, so every field is copied out by value.
        let base = ptr.read_unaligned();
        let float = match base.wFormatTag {
            WAVE_FORMAT_IEEE_FLOAT => true,
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_EXTENSIBLE => {
                let ext = (ptr as *const WAVEFORMATEXTENSIBLE).read_unaligned();
                match ext.SubFormat.data1 {
                    SUBTYPE_IEEE_FLOAT => true,
                    SUBTYPE_PCM => false,
                    other => bail!("unsupported WAVEFORMATEXTENSIBLE subformat {other}"),
                }
            }
            other => bail!("unsupported wave format tag {other:#x}"),
        };
        let kind = match (float, base.wBitsPerSample) {
            (true, 32) => SampleKind::F32,
            (false, 16) => SampleKind::I16,
            (false, 24) => SampleKind::I24,
            (false, 32) => SampleKind::I32,
            (_, bits) => bail!("unsupported sample size {bits} bits (float={float})"),
        };
        Ok(Self { sample_rate: base.nSamplesPerSec, channels: base.nChannels, kind })
    }
}

#[inline]
fn decode(bytes: &[u8], kind: SampleKind) -> f32 {
    match kind {
        SampleKind::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        SampleKind::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
        SampleKind::I24 => {
            let raw = (bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
            ((raw << 8) >> 8) as f32 / 8_388_608.0
        }
        SampleKind::I32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0,
    }
}

#[inline]
fn encode(sample: f32, out: &mut [u8], kind: SampleKind) {
    let x = sample.clamp(-1.0, 1.0);
    match kind {
        SampleKind::F32 => out.copy_from_slice(&x.to_le_bytes()),
        SampleKind::I16 => out.copy_from_slice(&((x * 32_767.0) as i16).to_le_bytes()),
        SampleKind::I24 => {
            let v = (x * 8_388_607.0) as i32;
            out.copy_from_slice(&v.to_le_bytes()[..3]);
        }
        SampleKind::I32 => out.copy_from_slice(&((x * 2_147_483_647.0) as i32).to_le_bytes()),
    }
}

/// Decodes `out.len()` frames from `src`, averaging the channels into mono.
pub fn read_mono(src: &[u8], out: &mut [f32], format: &StreamFormat) {
    let sb = format.kind.bytes();
    let channels = format.channels as usize;
    let fb = format.frame_bytes();
    debug_assert!(src.len() >= out.len() * fb);
    let gain = 1.0 / channels as f32;
    for (frame, dst) in out.iter_mut().enumerate() {
        let base = frame * fb;
        let sum: f32 = src[base..base + fb].chunks_exact(sb).map(|b| decode(b, format.kind)).sum();
        *dst = sum * gain;
    }
}

/// Writes mono samples into interleaved frames, duplicating each sample to every channel.
pub fn write_mono(dst: &mut [u8], mono: &[f32], format: &StreamFormat) {
    let sb = format.kind.bytes();
    let fb = format.frame_bytes();
    debug_assert!(dst.len() >= mono.len() * fb);
    for (frame, &sample) in mono.iter().enumerate() {
        let base = frame * fb;
        for slot in dst[base..base + fb].chunks_exact_mut(sb) {
            encode(sample, slot, format.kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [SampleKind; 4] = [SampleKind::F32, SampleKind::I16, SampleKind::I24, SampleKind::I32];

    fn fmt(kind: SampleKind, channels: u16) -> StreamFormat {
        StreamFormat { sample_rate: 48_000, channels, kind }
    }

    #[test]
    fn mono_round_trip_is_close_for_every_kind() {
        let input = [0.0f32, 0.25, -0.5, 0.999, -1.0, 0.1234];
        for kind in KINDS {
            let f = fmt(kind, 2);
            let mut bytes = vec![0u8; input.len() * f.frame_bytes()];
            write_mono(&mut bytes, &input, &f);
            let mut back = vec![0f32; input.len()];
            read_mono(&bytes, &mut back, &f);
            for (a, b) in input.iter().zip(&back) {
                assert!((a - b).abs() < 1e-3, "{kind:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn downmix_averages_channels() {
        let f = fmt(SampleKind::F32, 2);
        let stereo: Vec<u8> = [1.0f32, 0.0, -0.5, 0.5].iter().flat_map(|x| x.to_le_bytes()).collect();
        let mut mono = [0f32; 2];
        read_mono(&stereo, &mut mono, &f);
        assert_eq!(mono, [0.5, 0.0]);
    }

    #[test]
    fn upmix_duplicates_into_every_channel() {
        let f = fmt(SampleKind::I16, 3);
        let mut bytes = vec![0xAAu8; 3 * f.frame_bytes()];
        write_mono(&mut bytes, &[1.0], &f);
        assert_eq!(&bytes[0..2], &32_767i16.to_le_bytes());
        assert_eq!(&bytes[2..4], &32_767i16.to_le_bytes());
        assert_eq!(&bytes[4..6], &32_767i16.to_le_bytes());
    }

    #[test]
    fn out_of_range_and_nan_are_clamped_or_zeroed() {
        let f = fmt(SampleKind::I24, 1);
        let mut bytes = vec![0u8; 3 * 2];
        write_mono(&mut bytes, &[5.0, f32::NAN], &f);
        let mut back = [0f32; 2];
        read_mono(&bytes, &mut back, &f);
        assert!((back[0] - 1.0).abs() < 1e-3);
        assert_eq!(back[1], 0.0);
    }

    #[test]
    fn plain_waveformatex_fields_match_the_format() {
        let f = fmt(SampleKind::I24, 2);
        let wfx = f.to_waveformatex();
        let (tag, block, avg, bits) = (wfx.wFormatTag, wfx.nBlockAlign, wfx.nAvgBytesPerSec, wfx.wBitsPerSample);
        assert_eq!(tag, WAVE_FORMAT_PCM);
        assert_eq!(block, 6);
        assert_eq!(avg, 288_000);
        assert_eq!(bits, 24);
        let back = unsafe { StreamFormat::from_waveformatex(&wfx) }.unwrap();
        assert_eq!(back, f);
    }
}

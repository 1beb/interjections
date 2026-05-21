//! f32 → i16 PCM conversion with fixed gain and hard clamp.
//! Pocket TTS emits very quiet float audio (~1.6% full scale); apply a fixed
//! gain then clamp to avoid clipping. Streaming-safe (per-chunk, no global peak).

/// Convert a slice of f32 samples to i16 PCM, applying `gain` then clamping to
/// [-1.0, 1.0] before scaling. Clamp prevents wrap-around distortion.
pub fn to_i16_pcm(samples: &[f32], gain: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| ((s * gain).clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_maps_to_silence() {
        assert_eq!(to_i16_pcm(&[0.0, 0.0], 42.0), vec![0, 0]);
    }

    #[test]
    fn clamps_instead_of_wrapping() {
        assert_eq!(to_i16_pcm(&[10.0, -10.0], 42.0), vec![32767, -32767]);
    }

    #[test]
    fn applies_gain_and_scales() {
        // 0.01 * 42 = 0.42 → 0.42 * 32767 ≈ 13762
        assert_eq!(to_i16_pcm(&[0.01], 42.0), vec![13762]);
    }
}

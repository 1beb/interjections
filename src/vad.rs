pub struct EnergyVad {
    threshold: f32,
    min_speech_frames: u32,
    min_silence_frames: u32,
    triggered: bool,
    speaking: bool,
    speech_frames: u32,
    silence_frames: u32,
}

impl EnergyVad {
    pub fn new(threshold: f32, min_speech_ms: u32, min_silence_ms: u32, frame_ms: u32) -> Self {
        Self {
            threshold,
            min_speech_frames: min_speech_ms / frame_ms,
            min_silence_frames: min_silence_ms / frame_ms,
            triggered: false,
            speaking: false,
            speech_frames: 0,
            silence_frames: 0,
        }
    }

    pub fn process(&mut self, samples: &[i16]) -> bool {
        let energy = self.compute_energy(samples);
        let is_speech = energy >= self.threshold;

        if is_speech {
            self.speech_frames += 1;
            self.silence_frames = 0;
        } else {
            self.silence_frames += 1;
            self.speech_frames = 0;
        }

        if !self.triggered {
            if self.speech_frames >= self.min_speech_frames && is_speech {
                self.triggered = true;
                self.speaking = true;
                return true;
            }
            return false;
        }

        if self.silence_frames >= self.min_silence_frames && self.speaking {
            self.triggered = false;
            self.speaking = false;
            self.speech_frames = 0;
            self.silence_frames = 0;
            return false;
        }

        self.speaking
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    fn compute_energy(&self, samples: &[i16]) -> f32 {
        let sum: f64 = samples.iter()
            .map(|&s| (s as f64 / 32768.0).powi(2))
            .sum();
        (sum / samples.len() as f64).sqrt() as f32
    }
}

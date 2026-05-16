use crate::config::Config;

#[derive(Debug, Clone, PartialEq)]
pub enum CueType {
    Interjection,
    Correction,
    Backchannel,
    Append,
    Continue,
}

pub struct CueDetector {
    config: Config,
}

impl CueDetector {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn detect(&self, transcript: &str) -> CueType {
        let lower = transcript.to_lowercase().trim().to_string();

        for cue in &self.config.backchannel_cues {
            if lower == *cue || lower.starts_with(&format!("{} ", cue)) {
                return CueType::Backchannel;
            }
        }

        for cue in &self.config.interjection_cues {
            if lower.starts_with(cue) || format!(" {} ", lower).contains(&format!(" {} ", cue)) {
                return CueType::Interjection;
            }
        }

        for cue in &self.config.correction_signals {
            if lower.starts_with(cue) || format!(" {} ", lower).contains(&format!(" {} ", cue)) {
                return CueType::Correction;
            }
        }

        for cue in &self.config.append_signals {
            if lower.starts_with(cue) || format!(" {} ", lower).contains(&format!(" {} ", cue)) {
                return CueType::Append;
            }
        }

        CueType::Continue
    }
}

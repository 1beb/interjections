use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pocket_tts::{TTSModel, voice_state::ModelState};

use super::pcm::to_i16_pcm;

/// Local Pocket TTS. Model + voice loaded ONCE at startup; reused per utterance.
pub struct PocketTts {
    model: Arc<TTSModel>,
    voice: ModelState,
    gain: f32,
}

impl PocketTts {
    /// Load the model (weights resolved from the HF cache — see fetch script)
    /// and clone the default voice once. ~1.5 s.
    pub fn load(config: &crate::config::Config) -> anyhow::Result<Self> {
        let model = TTSModel::load_with_params(
            &config.pocket_variant,
            config.pocket_temperature,
            1, // lsd_decode_steps
            config.pocket_eos_threshold,
        )
        .map_err(|e| anyhow::anyhow!(
            "Pocket TTS model load failed: {e}. Run scripts/fetch-pocket-tts.sh \
             (needs HF_TOKEN + accepted license) to populate the model cache."
        ))?;
        let voice = model
            .get_voice_state(&config.pocket_voice)
            .map_err(|e| anyhow::anyhow!(
                "Pocket TTS voice load failed for '{}': {e}", config.pocket_voice
            ))?;
        Ok(Self { model: Arc::new(model), voice, gain: config.pocket_gain })
    }
}

#[async_trait::async_trait]
impl super::Tts for PocketTts {
    async fn speak(
        &self,
        text: &str,
        abort_flag: Arc<AtomicBool>,
        mut on_audio: Box<dyn FnMut(Vec<i16>) + Send>,
        on_done: Box<dyn FnOnce() + Send>,
    ) -> anyhow::Result<()> {
        let model = self.model.clone();
        let voice = self.voice.clone();
        let gain = self.gain;
        let text = text.to_string();

        let result = tokio::task::spawn_blocking(move || {
            for chunk in model.generate_stream(&text, &voice) {
                if abort_flag.load(Ordering::Relaxed) {
                    break;
                }
                let chunk = chunk?;
                let flat = chunk.flatten_all()?.to_vec1::<f32>()?;
                on_audio(to_i16_pcm(&flat, gain));
            }
            on_done();
            Ok::<(), anyhow::Error>(())
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(join_err) => Err(anyhow::anyhow!("Pocket TTS task panicked: {join_err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    // Only runs if the voice asset is present (implies the HF cache was populated
    // by scripts/fetch-pocket-tts.sh). Skipped otherwise — same gating style as
    // the pocket-tts crate's own integration tests.
    #[tokio::test]
    async fn pocket_tts_generates_speech_level_audio() {
        let voice = "data/models/pocket-tts/voice.wav";
        if !std::path::Path::new(voice).exists() {
            eprintln!("skipping: {voice} absent (run scripts/fetch-pocket-tts.sh)");
            return;
        }
        let mut cfg = crate::config::Config::default();
        cfg.tts_engine = "pocket".into();

        let tts = crate::tts::build(&cfg).expect("build pocket tts");
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i16>::new()));
        let c2 = collected.clone();

        tts.speak(
            "The quick brown fox jumps over the lazy dog.",
            abort,
            Box::new(move |s| c2.lock().unwrap().extend(s)),
            Box::new(|| {}),
        ).await.unwrap();

        let audio = collected.lock().unwrap();
        let peak = audio.iter().map(|s| s.abs() as i32).max().unwrap_or(0);
        assert!(audio.len() > 24_000, "expected >1s of audio, got {} samples", audio.len());
        assert!(peak > 3000, "expected speech-level peak, got {peak}");
    }
}

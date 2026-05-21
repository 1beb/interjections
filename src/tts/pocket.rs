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

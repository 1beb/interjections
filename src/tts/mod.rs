use std::sync::Arc;
use std::sync::atomic::AtomicBool;

mod cartesia;
mod pcm;

pub use cartesia::CartesiaTts;

/// A text-to-speech engine that streams i16 PCM chunks with abort support.
#[async_trait::async_trait]
pub trait Tts: Send + Sync {
    async fn speak(
        &self,
        text: &str,
        abort_flag: Arc<AtomicBool>,
        on_audio: Box<dyn FnMut(Vec<i16>) + Send>,
        on_done: Box<dyn FnOnce() + Send>,
    ) -> anyhow::Result<()>;
}

/// Build the configured TTS engine once, at startup.
pub fn build(config: &crate::config::Config) -> anyhow::Result<Arc<dyn Tts>> {
    match config.tts_engine.as_str() {
        "cartesia" => Ok(Arc::new(CartesiaTts::new(config.clone()))),
        // PocketTts is wired in Task 4; until then, selecting it errors clearly.
        "pocket" => anyhow::bail!("pocket TTS not yet wired (implemented in a later task)"),
        other => anyhow::bail!("unknown tts_engine '{other}' (expected 'pocket' or 'cartesia')"),
    }
}

use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Config;

const SHERPA_MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2";

pub struct LocalAsr {
    sherpa: Arc<Mutex<SherpaEngine>>,
}

pub struct AsrResult {
    pub text: String,
    pub is_final: bool,
    pub source: AsrSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AsrSource {
    Partial,
    Final,
}

struct SherpaEngine {
    recognizer: sherpa_onnx::OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
}

impl LocalAsr {
    pub async fn new(config: &Config) -> anyhow::Result<Self> {
        if !Path::new(&config.sherpa_model_dir).exists() {
            anyhow::bail!("Sherpa model not found at '{}'. Download manually:\n  {}", config.sherpa_model_dir, SHERPA_MODEL_URL);
        }

        let mut sherpa_config = sherpa_onnx::OnlineRecognizerConfig::default();
        sherpa_config.feat_config.sample_rate = config.sherpa_sample_rate as i32;
        sherpa_config.feat_config.feature_dim = 80;
        sherpa_config.model_config.transducer.encoder =
            Some(format!("{}/encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx", config.sherpa_model_dir));
        sherpa_config.model_config.transducer.decoder =
            Some(format!("{}/decoder-epoch-99-avg-1-chunk-16-left-128.onnx", config.sherpa_model_dir));
        sherpa_config.model_config.transducer.joiner =
            Some(format!("{}/joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx", config.sherpa_model_dir));
        sherpa_config.model_config.tokens =
            Some(format!("{}/tokens.txt", config.sherpa_model_dir));
        sherpa_config.enable_endpoint = true;
        sherpa_config.rule1_min_trailing_silence = 2.4;
        sherpa_config.rule2_min_trailing_silence = 1.2;
        sherpa_config.decoding_method = Some("greedy_search".into());

        let recognizer = sherpa_onnx::OnlineRecognizer::create(&sherpa_config)
            .ok_or_else(|| anyhow::anyhow!("Failed to create Sherpa recognizer"))?;
        let stream = recognizer.create_stream();

        Ok(Self {
            sherpa: Arc::new(Mutex::new(SherpaEngine { recognizer, stream })),
        })
    }

    pub async fn feed_audio(&self, samples: &[f32], sample_rate: u32) -> Vec<AsrResult> {
        let mut results = Vec::new();

        let sherpa_guard = self.sherpa.lock().await;
        sherpa_guard.stream.accept_waveform(sample_rate as i32, samples);

        while sherpa_guard.recognizer.is_ready(&sherpa_guard.stream) {
            sherpa_guard.recognizer.decode(&sherpa_guard.stream);
        }

        if let Some(result) = sherpa_guard.recognizer.get_result(&sherpa_guard.stream) {
            if !result.text.is_empty() {
                results.push(AsrResult {
                    text: result.text,
                    is_final: false,
                    source: AsrSource::Partial,
                });
            }
        }

        if sherpa_guard.recognizer.is_endpoint(&sherpa_guard.stream) {
            if let Some(result) = sherpa_guard.recognizer.get_result(&sherpa_guard.stream) {
                if !result.text.is_empty() {
                    results.push(AsrResult {
                        text: result.text,
                        is_final: true,
                        source: AsrSource::Final,
                    });
                }
            }
            sherpa_guard.recognizer.reset(&sherpa_guard.stream);
        }

        results
    }

    pub async fn reset(&self) {
        let mut guard = self.sherpa.lock().await;
        guard.stream = guard.recognizer.create_stream();
    }
}

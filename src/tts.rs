use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use base64::Engine;

use crate::config::Config;

pub struct CartesiaTts {
    config: Config,
}

impl CartesiaTts {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn speak(
        &self,
        text: &str,
        abort_flag: Arc<AtomicBool>,
        mut on_audio: impl FnMut(Vec<i16>) + Send + 'static,
        on_done: impl FnOnce() + Send + 'static,
    ) -> anyhow::Result<()> {
        let api_key = &self.config.cartesia_api_key;
        let ws_url = format!(
            "{}?api_key={}&cartesia_version=2026-03-01",
            self.config.cartesia_ws_url,
            api_key,
        );

        let (mut ws, _) = connect_async(&ws_url).await?;
        log::info!("TTS WebSocket connected");

        let context_id = uuid::Uuid::new_v4().to_string();

        let request = serde_json::json!({
            "context_id": context_id,
            "model_id": self.config.cartesia_model_id,
            "transcript": text,
            "voice": {
                "mode": "id",
                "id": self.config.cartesia_voice_id,
            },
            "output_format": {
                "container": "raw",
                "encoding": "pcm_s16le",
                "sample_rate": self.config.tts_sample_rate,
            },
            "continue": false,
            "add_timestamps": false,
        });

        ws.send(Message::Text(request.to_string())).await?;
        log::info!("TTS request sent, context_id={}", context_id);

        let engine = base64::engine::general_purpose::STANDARD;

        loop {
            if abort_flag.load(Ordering::Relaxed) {
                log::info!("TTS aborted");
                ws.close(None).await.ok();
                return Ok(());
            }

            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&t) {
                        let msg_type = val["type"].as_str().unwrap_or("");

                        match msg_type {
                            "chunk" => {
                                if let Some(data_b64) = val["data"].as_str() {
                                    if let Ok(bytes) = engine.decode(data_b64) {
                                        let samples: Vec<i16> = bytes.chunks(2)
                                            .filter(|c| c.len() == 2)
                                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                            .collect();
                                        if !samples.is_empty() {
                                            on_audio(samples);
                                        }
                                    }
                                }
                            }
                            "done" => {
                                log::info!("TTS done signal received");
                                break;
                            }
                            "error" => {
                                let error_msg = val["message"].as_str().unwrap_or("unknown error");
                                log::error!("TTS error: {}", error_msg);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Some(Ok(Message::Binary(data))) => {
                    let samples: Vec<i16> = data.chunks(2)
                        .filter(|c| c.len() == 2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    if !samples.is_empty() {
                        on_audio(samples);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    log::error!("TTS WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        ws.close(None).await.ok();
        on_done();
        Ok(())
    }
}
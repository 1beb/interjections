mod config;
mod audio;
mod vad;
mod local_asr;
mod tts;
mod cues;
mod reconciler;
mod controller;
mod web;

use clap::Parser;
use config::{Cli, Config};
use controller::{Controller, State};
use local_asr::LocalAsr;
use audio::{NoiseSuppressor, AudioMessage};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let config = Config::from_cli(&cli)?;

    if config.debug {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
            .init();
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .init();
    }

    log::info!("interjections v{}", env!("CARGO_PKG_VERSION"));
    log::info!("Sherpa model dir: {}", config.sherpa_model_dir);

    let web_channels = if cli.web {
        let (audio_tx, _) = tokio::sync::broadcast::channel::<Vec<i16>>(128);
        let (state_tx, _) = tokio::sync::broadcast::channel::<String>(64);
        let (transcript_tx, _) = tokio::sync::broadcast::channel::<(String, String)>(64);
        let (log_tx, _) = tokio::sync::broadcast::channel::<String>(64);
        Some(controller::WebChannels { audio_tx, state_tx, transcript_tx, log_tx })
    } else {
        None
    };

    let config = Arc::new(config);
    let controller = Arc::new(Controller::new((*config).clone(), web_channels.clone()));

    let asr = LocalAsr::new(&config).await?;
    log::info!("Local ASR initialized (Sherpa-onnx)");

    let asr_for_audio = Arc::new(Mutex::new(asr));
    let controller_for_audio = controller.clone();
    let config_audio = config.clone();
    let target_rate = config_audio.sample_rate;

    let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<AudioMessage>(1024);

    if cli.web {
        let web_config = (*config).clone();
        let channels = web_channels.clone().unwrap();
        let audio_tx_for_web = audio_tx.clone();
        let target_rate_web = target_rate;

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                web::start(web_config, channels, audio_tx_for_web).await.ok();
            });
        });

        log::info!("Web interface at https://{}:{}", config.web_host, config.web_port);
    } else {
        // Terminal mode: capture from local mic
        let audio_tx_capture = audio_tx.clone();
        let config_capture = config_audio.clone();

        std::thread::spawn(move || {
            let mut vad = vad::EnergyVad::new(
                config_capture.vad_threshold,
                config_capture.vad_min_speech_ms,
                config_capture.vad_min_silence_ms,
                config_capture.chunk_ms,
            );
            let mut noise_suppressor = NoiseSuppressor::new();

            let _capture = audio::AudioCapture::new(
                target_rate,
                config_capture.chunk_ms,
                move |samples, actual_rate| {
                    let resampled = if actual_rate != target_rate {
                        audio::resample(&samples, actual_rate, target_rate)
                    } else {
                        samples
                    };

                    let clean = noise_suppressor.process(&resampled);
                    let float_samples: Vec<f32> = clean.iter()
                        .map(|&s| s as f32 / 32768.0)
                        .collect();

                    let vad_speaking = vad.process(&clean);

                    if let Err(_) = audio_tx_capture.blocking_send(AudioMessage::Chunk { samples: float_samples, vad_speaking }) {
                        return;
                    }
                },
            );
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        });
    }

    let asr_task = asr_for_audio.clone();
    let controller_task = controller_for_audio.clone();
    let config_task = config.clone();

    tokio::spawn(async move {
        let mut was_speaking = false;

        while let Some(msg) = audio_rx.recv().await {
            let AudioMessage::Chunk { samples, vad_speaking } = msg;

            if vad_speaking && !was_speaking {
                let state = *controller_task.state.lock().await;
                match state {
                    State::Idle => {
                        *controller_task.state.lock().await = State::User;
                        log::info!("State -> User (VAD start)");
                        controller_task.broadcast_state(State::User);
                    }
                    State::User | State::Thinking => {}
                }
            }

            if was_speaking && !vad_speaking {
                let state = *controller_task.state.lock().await;
                if state == State::User {
                    log::info!("VAD silence while in User state — will use ASR endpoint instead");
                }
            }

            was_speaking = vad_speaking;

            let results = asr_task.lock().await.feed_audio(&samples, config_task.sample_rate).await;

            for result in results {
                controller_task.on_asr_result(result).await;
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    log::info!("Shutting down");
    Ok(())
}

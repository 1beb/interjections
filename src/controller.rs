use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::config::Config;
use crate::cues::{CueDetector, CueType};
use crate::reconciler::{ContextReconciler, TurnType};
use crate::local_asr::{AsrResult, AsrSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    User,
    Gating,
    Thinking,
}

pub struct Controller {
    pub config: Config,
    pub state: Arc<Mutex<State>>,
    reconciler: Arc<Mutex<ContextReconciler>>,
    cue_detector: CueDetector,
    partial_transcript: Arc<Mutex<String>>,
    web_channels: Option<WebChannels>,
    gate_tx: Option<mpsc::UnboundedSender<()>>,
}

#[derive(Clone)]
pub struct WebChannels {
    pub audio_tx: tokio::sync::broadcast::Sender<Vec<i16>>,
    pub state_tx: tokio::sync::broadcast::Sender<String>,
    pub transcript_tx: tokio::sync::broadcast::Sender<(String, String)>,
    pub log_tx: tokio::sync::broadcast::Sender<String>,
}

impl Controller {
    pub fn new(
        config: Config,
        web_channels: Option<WebChannels>,
        gate_tx: Option<mpsc::UnboundedSender<()>>,
    ) -> Self {
        Self {
            cue_detector: CueDetector::new(config.clone()),
            reconciler: Arc::new(Mutex::new(ContextReconciler::new())),
            state: Arc::new(Mutex::new(State::Idle)),
            config,
            partial_transcript: Arc::new(Mutex::new(String::new())),
            web_channels,
            gate_tx,
        }
    }

    pub fn broadcast_state(&self, s: State) {
        if let Some(ref ch) = self.web_channels {
            let label = match s {
                State::Idle => "idle",
                State::User => "user",
                State::Gating => "gating",
                State::Thinking => "thinking",
            };
            let _ = ch.state_tx.send(label.to_string());
        }
    }

    fn broadcast_transcript(&self, text: &str, kind: &str) {
        if let Some(ref ch) = self.web_channels {
            let _ = ch.transcript_tx.send((text.to_string(), kind.to_string()));
        }
    }

    fn broadcast_log(&self, msg: &str) {
        if let Some(ref ch) = self.web_channels {
            let _ = ch.log_tx.send(msg.to_string());
        }
    }

    pub async fn on_asr_result(&self, result: AsrResult) {
        *self.partial_transcript.lock().await = result.text.clone();

        let state = *self.state.lock().await;

        match result.source {
            AsrSource::Partial => {
                self.broadcast_transcript(&result.text, "partial");
                let cue = self.cue_detector.detect(&result.text);
                if matches!(cue, CueType::Interjection | CueType::Correction) {
                    if state == State::Thinking {
                        let mut rec = self.reconciler.lock().await;
                        rec.add_turn(TurnType::Interjection, result.text.clone(),
                                     Some(format!("{:?}", cue)));
                        log::info!("[ASR] Interjection: {}", result.text);
                    }
                }
            }
            AsrSource::Final => {
                if !result.text.trim().is_empty() {
                    let mut rec = self.reconciler.lock().await;
                    rec.add_turn(TurnType::User, result.text.clone(), None);
                    log::info!("[ASR] Final: {}", result.text);
                }
                match &self.gate_tx {
                    // Gate wired: append-only; the listening task decides.
                    Some(tx) => { let _ = tx.send(()); }
                    // Not yet wired (pre-Task-7): old inline-submit behaviour.
                    None => {
                        let current_state = *self.state.lock().await;
                        if current_state != State::Thinking {
                            self.on_speech_end().await;
                        }
                    }
                }
            }
        }
    }

    pub async fn on_speech_end(&self) {
        log::info!("on_speech_end called");
        {
            let mut state = self.state.lock().await;
            *state = State::Thinking;
        }
        self.broadcast_state(State::Thinking);

        let reconciled = {
            let mut rec = self.reconciler.lock().await;
            let r = rec.reconcile();
            rec.reset();
            r
        };

        log::info!("reconciled text: '{}'", reconciled);

        if reconciled.trim().is_empty() {
            log::info!("reconciled empty, bailing");
            *self.state.lock().await = State::Idle;
            self.broadcast_state(State::Idle);
            return;
        }

        log::info!("broadcasting submit");
        self.broadcast_transcript(&reconciled, "submit");
        self.broadcast_log(&format!("submitting: {}", reconciled));
        log::info!("Submitting to OpenCode: {}", reconciled);

        *self.state.lock().await = State::Idle;
        self.broadcast_state(State::Idle);
    }
}

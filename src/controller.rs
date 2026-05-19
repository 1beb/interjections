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

    /// Shared handle to the reconciler buffer, for the listening task.
    pub fn reconciler_handle(&self) -> Arc<Mutex<ContextReconciler>> {
        self.reconciler.clone()
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

use crate::gate::{Gate, GateLoop, LoopAction, Verdict};
use tokio::time::{sleep, sleep_until, Duration, Instant};

#[derive(Clone, Copy)]
enum Patience {
    /// hold:false — on expiry, force-commit the buffer as a prompt.
    Fallback,
    /// hold:true — on expiry, an abandoned session: discard the buffer.
    HoldBackstop,
}

/// Spawn the long-lived listening task. It owns the gate call, the re-listen
/// loop, and the safety-net timers (spec section 4.4).
pub fn spawn_listening_task(
    mut rx: mpsc::UnboundedReceiver<()>,
    state: Arc<Mutex<State>>,
    reconciler: Arc<Mutex<ContextReconciler>>,
    channels: Option<WebChannels>,
    gate: Gate,
    config: Config,
) {
    tokio::spawn(async move {
        let mut gl = GateLoop::new(config.gate_max_rechecks);
        // None = idle (block on a signal). Some = waiting after `incomplete`.
        let mut armed: Option<(Instant, Patience)> = None;

        loop {
            // --- 1. Wait for a signal, or for an armed patience timer. ---
            let got_signal = match armed {
                Some((deadline, _)) => tokio::select! {
                    s = rx.recv() => match s { Some(()) => true, None => return },
                    _ = sleep_until(deadline) => false,
                },
                None => match rx.recv().await { Some(()) => true, None => return },
            };

            if !got_signal {
                // Timer expired with no new speech.
                match armed.take().expect("armed").1 {
                    Patience::Fallback => {
                        log::info!("[gate] silence fallback -> commit");
                        commit(&state, &reconciler, &channels, &mut gl).await;
                    }
                    Patience::HoldBackstop => {
                        log::info!("[gate] hold backstop -> discard abandoned buffer");
                        discard(&state, &reconciler, &mut gl).await;
                    }
                }
                continue;
            }
            armed = None;

            // --- 2. Debounce: coalesce a burst of rapid finals. ---
            sleep(Duration::from_millis(config.gate_debounce_ms)).await;
            while rx.try_recv().is_ok() {}

            // --- 3-6. Snapshot, classify, dirty-recheck, act. ---
            'regate: loop {
                set_state(&state, &channels, State::Gating, "checking").await;

                let text = { reconciler.lock().await.reconcile() };
                if text.trim().is_empty() {
                    set_state(&state, &channels, State::Idle, "idle").await;
                    break 'regate;
                }

                let verdict = if config.no_gate {
                    Verdict::Prompt
                } else {
                    gate.classify(&text).await
                };

                // dirty: the user spoke more while the call was in flight.
                let mut dirty = false;
                while rx.try_recv().is_ok() { dirty = true; }
                if dirty {
                    continue 'regate; // re-snapshot the now-larger buffer
                }

                match gl.next_action(&verdict) {
                    LoopAction::Commit => {
                        commit(&state, &reconciler, &channels, &mut gl).await;
                    }
                    LoopAction::HoldIndefinite => {
                        set_state(&state, &channels, State::Gating, "listening").await;
                        armed = Some((
                            Instant::now() + Duration::from_millis(config.gate_hold_backstop_ms),
                            Patience::HoldBackstop,
                        ));
                    }
                    LoopAction::ArmFallback => {
                        set_state(&state, &channels, State::Gating, "go-on").await;
                        armed = Some((
                            Instant::now() + Duration::from_millis(config.gate_silence_fallback_ms),
                            Patience::Fallback,
                        ));
                    }
                }
                break 'regate;
            }
        }
    });
}

/// Set the state enum and broadcast a (possibly more specific) label.
async fn set_state(
    state: &Arc<Mutex<State>>,
    channels: &Option<WebChannels>,
    s: State,
    label: &str,
) {
    *state.lock().await = s;
    if let Some(ch) = channels {
        let _ = ch.state_tx.send(label.to_string());
    }
}

/// Commit the buffer: broadcast submit, reset the reconciler and the loop.
async fn commit(
    state: &Arc<Mutex<State>>,
    reconciler: &Arc<Mutex<ContextReconciler>>,
    channels: &Option<WebChannels>,
    gl: &mut GateLoop,
) {
    let text = {
        let mut rec = reconciler.lock().await;
        // Re-read the freshest buffer: the dirty-check in the re-listen loop
        // guarantees no un-signalled turns arrived since the classify snapshot.
        let t = rec.reconcile();
        rec.reset();
        t
    };
    gl.reset();
    if text.trim().is_empty() {
        set_state(state, channels, State::Idle, "idle").await;
        return;
    }
    log::info!("[gate] commit -> submit: {}", text);
    if let Some(ch) = channels {
        // The "submit" transcript drives injection + the SSE/TTS listener in
        // web.rs. The "thinking" state is a brief visual; Plan 1 has no
        // barge-in, so we immediately return to Idle below — the widget shows
        // "thinking..." then "connected". This double state-send is intended.
        let _ = ch.transcript_tx.send((text.clone(), "submit".to_string()));
        let _ = ch.state_tx.send("thinking".to_string());
    }
    set_state(state, channels, State::Idle, "idle").await;
}

/// Discard the buffer without submitting (abandoned hold:true session).
async fn discard(
    state: &Arc<Mutex<State>>,
    reconciler: &Arc<Mutex<ContextReconciler>>,
    gl: &mut GateLoop,
) {
    reconciler.lock().await.reset();
    gl.reset();
    *state.lock().await = State::Idle;
}

#[cfg(test)]
mod listening_tests {
    use super::*;
    use crate::gate::Gate;
    use crate::reconciler::TurnType;

    // Build a mock gate that returns a fixed sequence of verdict JSONs, one
    // per call (repeating the last once exhausted).
    async fn spawn_mock_seq(responses: Vec<&'static str>) -> String {
        use axum::{routing::post, Json, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let responses = Arc::new(responses);
        let counter = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let responses = responses.clone();
                let counter = counter.clone();
                async move {
                    let i = counter.fetch_add(1, Ordering::SeqCst).min(responses.len() - 1);
                    Json(serde_json::json!({
                        "choices": [{"message": {"content": responses[i]}}]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        format!("http://{}/v1/chat/completions", addr)
    }

    // Spin up a listening task wired to a mock gate. Returns the signal sender,
    // the shared reconciler, and a transcript receiver to observe commits.
    async fn harness(
        responses: Vec<&'static str>,
        tweak: impl FnOnce(&mut Config),
    ) -> (
        mpsc::UnboundedSender<()>,
        Arc<Mutex<ContextReconciler>>,
        tokio::sync::broadcast::Receiver<(String, String)>,
    ) {
        let mut config = Config::default();
        config.gate_endpoint = spawn_mock_seq(responses).await;
        config.gate_debounce_ms = 20;
        config.gate_timeout_ms = 2000;
        tweak(&mut config);

        let (audio_tx, _) = tokio::sync::broadcast::channel(16);
        let (state_tx, _) = tokio::sync::broadcast::channel(16);
        let (transcript_tx, transcript_rx) = tokio::sync::broadcast::channel(16);
        let (log_tx, _) = tokio::sync::broadcast::channel(16);
        let channels = WebChannels { audio_tx, state_tx, transcript_tx, log_tx };

        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(State::Idle));
        let reconciler = Arc::new(Mutex::new(ContextReconciler::new()));
        let gate = Gate::new(&config);

        spawn_listening_task(rx, state, reconciler.clone(), Some(channels), gate, config);
        (tx, reconciler, transcript_rx)
    }

    // Push a finalized ASR segment: append to the reconciler, then signal.
    async fn say(
        tx: &mpsc::UnboundedSender<()>,
        rec: &Arc<Mutex<ContextReconciler>>,
        text: &str,
    ) {
        rec.lock().await.add_turn(TurnType::User, text.to_string(), None);
        tx.send(()).unwrap();
    }

    // Wait briefly for a "submit" broadcast; None if none arrives in time.
    async fn next_submit(
        rx: &mut tokio::sync::broadcast::Receiver<(String, String)>,
        ms: u64,
    ) -> Option<String> {
        let deadline = tokio::time::Duration::from_millis(ms);
        loop {
            match tokio::time::timeout(deadline, rx.recv()).await {
                Ok(Ok((text, kind))) if kind == "submit" => return Some(text),
                Ok(Ok(_)) => continue, // a non-submit kind (e.g. "partial") — ignore
                _ => return None,
            }
        }
    }

    #[tokio::test]
    async fn waits_on_hold_true_then_commits_accumulated_thought() {
        let (tx, rec, mut sub) = harness(
            vec![
                r#"{"status":"incomplete","hold":true}"#,
                r#"{"status":"prompt"}"#,
            ],
            |_| {},
        ).await;

        say(&tx, &rec, "I'm going to tell you a story").await;
        // hold:true -> nothing is submitted, however long we wait.
        assert_eq!(next_submit(&mut sub, 300).await, None);

        say(&tx, &rec, "about a race condition").await;
        // Second pause -> prompt -> the assembled thought is submitted once.
        assert_eq!(
            next_submit(&mut sub, 500).await.as_deref(),
            Some("I'm going to tell you a story about a race condition"),
        );
    }

    #[tokio::test]
    async fn hold_false_force_commits_via_silence_fallback() {
        let (tx, rec, mut sub) = harness(
            vec![r#"{"status":"incomplete","hold":false}"#],
            |c| c.gate_silence_fallback_ms = 200,
        ).await;

        say(&tx, &rec, "open the").await;
        // No more speech: the silence fallback fires and commits what we have.
        assert_eq!(next_submit(&mut sub, 800).await.as_deref(), Some("open the"));
    }

    #[tokio::test]
    async fn hold_true_backstop_discards_abandoned_buffer() {
        let (tx, rec, mut sub) = harness(
            vec![r#"{"status":"incomplete","hold":true}"#],
            |c| c.gate_hold_backstop_ms = 150,
        ).await;

        say(&tx, &rec, "I'm going to tell you a story").await;
        // hold:true with no follow-up speech: the backstop fires and the
        // abandoned buffer is discarded — nothing is ever submitted.
        assert_eq!(next_submit(&mut sub, 500).await, None);
        // And the reconciler buffer was reset by discard().
        assert!(rec.lock().await.reconcile().trim().is_empty());
    }
}

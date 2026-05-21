//! The gate: a fast LLM that decides whether a spoken utterance is a complete
//! thought, between ASR and submit. See docs/2026-05-16-...-design.md section 4.

// The command-mode items (CommandAction, Verdict::Command) are not exercised
// until Plan 2; the gate/listening path itself is fully wired. Keep the allow
// until command mode lands.
#![allow(dead_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    NewSession,
    OpenFile,
    SwitchModel,
    SwitchProject,
    SwitchSession,
    CycleVariant,
    RunCommand,
}

impl CommandAction {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "new_session" => Self::NewSession,
            "open_file" => Self::OpenFile,
            "switch_model" => Self::SwitchModel,
            "switch_project" => Self::SwitchProject,
            "switch_session" => Self::SwitchSession,
            "cycle_variant" => Self::CycleVariant,
            "run_command" => Self::RunCommand,
            _ => return None,
        })
    }
    /// Actions that take no target (`target` must be null/absent).
    fn targetless(&self) -> bool {
        matches!(self, Self::NewSession | Self::CycleVariant)
    }
}

/// The gate's verdict on one accumulated utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The user is not done. `hold` true = a confident preface (wait
    /// indefinitely); false = an ambiguous fragment (arm the safety net).
    Incomplete { hold: bool },
    /// A complete thought for the assistant. Submit the reconciled text.
    Prompt,
    /// A UI command. (Not requested by the Plan 1 prompt; handled in Plan 2.)
    Command { action: CommandAction, target: Option<String> },
}

use std::time::Duration;

/// Plan 1 gate prompt — completeness only. Plan 2 extends this with the
/// adapter command vocabulary; `parse_verdict` already handles `command`.
pub const GATE_SYSTEM_PROMPT: &str = "\
You are a fast gate between speech-to-text and a coding assistant.
Input: a raw ASR transcript of something a user said aloud.
Output ONLY a JSON object — no prose, no code fences.

status:
  \"incomplete\" — the user is NOT done. Judge the *thought*, not grammar:
                 a fragment (\"open the\"), a trail-off (\"and then, um\"), OR a
                 preface that promises more (\"I'm going to tell you a story\",
                 \"okay so here's what I want\", \"let me explain\").
                 Return ONLY {\"status\":\"incomplete\",\"hold\":<bool>}.
                   hold=true  — the user explicitly signalled more is coming.
                   hold=false — an ambiguous fragment or trail-off.
  \"prompt\"     — the user finished a complete thought. Return {\"status\":\"prompt\"}.

When unsure, choose \"prompt\". A grammatically whole sentence can still be
incomplete: a preface is a promise of more.";

pub struct Gate {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    reasoning_effort: String,
    timeout: Duration,
}

/// Build the OpenAI-compatible chat-completion request body.
///
/// `reasoning_effort` is the per-endpoint no-think switch: "low" for gpt-oss
/// (Cerebras, the default), "none" for ollama qwen. `max_tokens` must leave room
/// for any reasoning preamble plus the JSON verdict — gpt-oss at "low" emits a
/// short reasoning trace (~150 chars) before the JSON, so a tight 80-token cap
/// would truncate it.
fn build_request(model: &str, text: &str, reasoning_effort: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": GATE_SYSTEM_PROMPT},
            {"role": "user", "content": text},
        ],
        "temperature": 0,
        "max_tokens": 512,
        "response_format": {"type": "json_object"},
        "reasoning_effort": reasoning_effort,
    })
}

impl Gate {
    pub fn new(config: &crate::config::Config) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(config.gate_timeout_ms))
                .build()
                .expect("reqwest client"),
            endpoint: config.gate_endpoint.clone(),
            model: config.gate_model.clone(),
            api_key: config.gate_api_key.clone(),
            reasoning_effort: config.gate_reasoning_effort.clone(),
            timeout: Duration::from_millis(config.gate_timeout_ms),
        }
    }

    /// Classify one accumulated utterance. Fail-open: any error returns
    /// `Verdict::Prompt` (spec section 4.7).
    pub async fn classify(&self, text: &str) -> Verdict {
        let body = build_request(&self.model, text, &self.reasoning_effort);
        let mut req = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = match tokio::time::timeout(self.timeout, req.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => { log::warn!("[gate] request error: {e}"); return Verdict::Prompt; }
            Err(_) => { log::warn!("[gate] timed out"); return Verdict::Prompt; }
        };
        let value: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => { log::warn!("[gate] bad response body: {e}"); return Verdict::Prompt; }
        };
        let content = value["choices"][0]["message"]["content"].as_str().unwrap_or("");
        let verdict = parse_verdict(content);
        log::info!("[gate] '{}' -> {:?}", text, verdict);
        verdict
    }
}

/// Parse the gate model's reply into a `Verdict`. Fail-safe: any malformed or
/// unexpected content returns `Verdict::Prompt` (spec section 4.7).
pub fn parse_verdict(content: &str) -> Verdict {
    // Models occasionally wrap JSON in ``` fences or add prose — extract the
    // first balanced-looking {...} span.
    let json = match (content.find('{'), content.rfind('}')) {
        (Some(a), Some(b)) if b > a => &content[a..=b],
        _ => return Verdict::Prompt,
    };
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Verdict::Prompt,
    };
    match v["status"].as_str() {
        Some("incomplete") => Verdict::Incomplete {
            hold: v["hold"].as_bool().unwrap_or(false),
        },
        Some("prompt") => Verdict::Prompt,
        Some("command") => {
            let action = match v["command"]["action"].as_str().and_then(CommandAction::from_str) {
                Some(a) => a,
                None => return Verdict::Prompt,
            };
            let target = v["command"]["target"].as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            // Reject a command whose target presence does not match its action.
            if action.targetless() {
                if target.is_some() { return Verdict::Prompt; }
            } else if target.is_none() {
                return Verdict::Prompt;
            }
            Verdict::Command { action, target }
        }
        _ => Verdict::Prompt,
    }
}

/// What the listening task should do after a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAction {
    /// Commit the buffer now (broadcast submit).
    Commit,
    /// `hold:true` — wait indefinitely; arm only the abandoned-session backstop.
    HoldIndefinite,
    /// `hold:false` — arm the silence fallback timer.
    ArmFallback,
}

/// Tracks consecutive `hold:false` verdicts so the task can force a commit
/// when the gate is repeatedly uncertain (spec section 4.4).
pub struct GateLoop {
    recheck_count: u32,
    max_rechecks: u32,
}

impl GateLoop {
    pub fn new(max_rechecks: u32) -> Self {
        Self { recheck_count: 0, max_rechecks }
    }

    /// Call once per verdict. Returns the action; mutates the re-check counter.
    pub fn next_action(&mut self, verdict: &Verdict) -> LoopAction {
        match verdict {
            Verdict::Incomplete { hold: true } => LoopAction::HoldIndefinite,
            Verdict::Incomplete { hold: false } => {
                self.recheck_count += 1;
                if self.recheck_count >= self.max_rechecks {
                    LoopAction::Commit
                } else {
                    LoopAction::ArmFallback
                }
            }
            Verdict::Prompt | Verdict::Command { .. } => LoopAction::Commit,
        }
    }

    /// Reset after a commit.
    pub fn reset(&mut self) {
        self.recheck_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_hold_true() {
        assert_eq!(parse_verdict(r#"{"status":"incomplete","hold":true}"#),
                   Verdict::Incomplete { hold: true });
    }

    #[test]
    fn incomplete_hold_false() {
        assert_eq!(parse_verdict(r#"{"status":"incomplete","hold":false}"#),
                   Verdict::Incomplete { hold: false });
    }

    #[test]
    fn incomplete_missing_hold_defaults_false() {
        // A missing `hold` is treated as false — fail-safe, never hang (4.3).
        assert_eq!(parse_verdict(r#"{"status":"incomplete"}"#),
                   Verdict::Incomplete { hold: false });
    }

    #[test]
    fn prompt_status() {
        assert_eq!(parse_verdict(r#"{"status":"prompt"}"#), Verdict::Prompt);
    }

    #[test]
    fn command_with_target() {
        assert_eq!(
            parse_verdict(r#"{"status":"command","command":{"action":"open_file","target":"auth.rs"}}"#),
            Verdict::Command { action: CommandAction::OpenFile, target: Some("auth.rs".into()) });
    }

    #[test]
    fn command_targetless() {
        assert_eq!(
            parse_verdict(r#"{"status":"command","command":{"action":"new_session","target":null}}"#),
            Verdict::Command { action: CommandAction::NewSession, target: None });
    }

    #[test]
    fn command_missing_required_target_fails_open() {
        // open_file requires a target; absent => reject => Prompt (4.2).
        assert_eq!(
            parse_verdict(r#"{"status":"command","command":{"action":"open_file","target":null}}"#),
            Verdict::Prompt);
    }

    #[test]
    fn json_in_code_fences_is_tolerated() {
        assert_eq!(parse_verdict("```json\n{\"status\":\"prompt\"}\n```"), Verdict::Prompt);
    }

    #[test]
    fn malformed_json_fails_open_to_prompt() {
        assert_eq!(parse_verdict("not json at all"), Verdict::Prompt);
        assert_eq!(parse_verdict(""), Verdict::Prompt);
        assert_eq!(parse_verdict(r#"{"status":"banana"}"#), Verdict::Prompt);
    }

    #[test]
    fn build_request_shape() {
        let r = build_request("gpt-oss-120b", "open the lexer", "low");
        assert_eq!(r["model"], "gpt-oss-120b");
        assert_eq!(r["temperature"], 0);
        assert_eq!(r["response_format"]["type"], "json_object");
        assert_eq!(r["reasoning_effort"], "low");
        assert_eq!(r["messages"][0]["role"], "system");
        assert_eq!(r["messages"][1]["role"], "user");
        assert_eq!(r["messages"][1]["content"], "open the lexer");
    }

    #[test]
    fn build_request_respects_reasoning_effort() {
        // ollama qwen path still works with the "none" switch.
        let r = build_request("qwen3.5:4b", "hi", "none");
        assert_eq!(r["reasoning_effort"], "none");
    }

    /// Spawn a mock OpenAI-compatible endpoint that returns the given verdict
    /// JSON as the message content. Returns the chat-completions URL.
    pub(crate) async fn spawn_mock_gate(content: &'static str) -> String {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || async move {
                Json(serde_json::json!({
                    "choices": [{"message": {"content": content}}]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        format!("http://{}/v1/chat/completions", addr)
    }

    fn test_config(endpoint: String) -> crate::config::Config {
        let mut c = crate::config::Config::default();
        c.gate_endpoint = endpoint;
        c.gate_timeout_ms = 2000;
        c
    }

    #[tokio::test]
    async fn classify_parses_a_verdict() {
        let url = spawn_mock_gate(r#"{"status":"incomplete","hold":true}"#).await;
        let gate = Gate::new(&test_config(url));
        assert_eq!(gate.classify("I'm going to tell you a story").await,
                   Verdict::Incomplete { hold: true });
    }

    #[tokio::test]
    async fn classify_fails_open_when_unreachable() {
        // Nothing listening on this port -> connection refused -> Prompt.
        let gate = Gate::new(&test_config("http://127.0.0.1:1/v1/chat/completions".into()));
        assert_eq!(gate.classify("anything").await, Verdict::Prompt);
    }

    #[test]
    fn prompt_commits() {
        let mut gl = GateLoop::new(3);
        assert_eq!(gl.next_action(&Verdict::Prompt), LoopAction::Commit);
    }

    #[test]
    fn command_commits() {
        let mut gl = GateLoop::new(3);
        let v = Verdict::Command { action: CommandAction::NewSession, target: None };
        assert_eq!(gl.next_action(&v), LoopAction::Commit);
    }

    #[test]
    fn hold_true_waits_indefinitely_and_does_not_advance_counter() {
        let mut gl = GateLoop::new(3);
        for _ in 0..10 {
            assert_eq!(gl.next_action(&Verdict::Incomplete { hold: true }),
                       LoopAction::HoldIndefinite);
        }
    }

    #[test]
    fn hold_false_arms_fallback_then_force_commits_at_cap() {
        let mut gl = GateLoop::new(3);
        let f = Verdict::Incomplete { hold: false };
        assert_eq!(gl.next_action(&f), LoopAction::ArmFallback); // 1
        assert_eq!(gl.next_action(&f), LoopAction::ArmFallback); // 2
        assert_eq!(gl.next_action(&f), LoopAction::Commit);      // 3 -> cap
    }

    #[test]
    fn reset_clears_the_recheck_counter() {
        let mut gl = GateLoop::new(3);
        let f = Verdict::Incomplete { hold: false };
        gl.next_action(&f);
        gl.next_action(&f);
        gl.reset();
        assert_eq!(gl.next_action(&f), LoopAction::ArmFallback); // counter back to 1
    }
}

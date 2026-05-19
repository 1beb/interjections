//! The gate: a fast LLM that decides whether a spoken utterance is a complete
//! thought, between ASR and submit. See docs/2026-05-16-...-design.md section 4.

// Items here are unused until the listening task is wired in Task 7. Inner
// attribute (not an outer attr on `mod gate;`, which would not reach these).
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
}

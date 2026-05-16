use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnType {
    User,
    Model,
    Interjection,
    Correction,
    Backchannel,
    Append,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnStatus {
    Active,
    Superseded,
    Signal,
    Filtered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub turn_type: TurnType,
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub status: TurnStatus,
    pub corrected_by: Vec<String>,
    pub signal: Option<String>,
}

pub struct ContextReconciler {
    turns: Vec<Turn>,
    counter: u64,
}

impl ContextReconciler {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            counter: 0,
        }
    }

    pub fn add_turn(&mut self, turn_type: TurnType, text: String, signal: Option<String>) -> Turn {
        self.counter += 1;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let id = format!("turn_{:04}", self.counter);

        let mut turn = Turn {
            id,
            turn_type: turn_type.clone(),
            text,
            start_time: now,
            end_time: now,
            status: TurnStatus::Active,
            corrected_by: Vec::new(),
            signal,
        };

        match turn_type {
            TurnType::Backchannel => {
                turn.status = TurnStatus::Filtered;
                self.turns.push(turn.clone());
            }
            TurnType::Correction => {
                turn.status = TurnStatus::Signal;
                for t in self.turns.iter_mut().rev() {
                    if t.turn_type == TurnType::User && t.status == TurnStatus::Active {
                        t.status = TurnStatus::Superseded;
                        t.corrected_by.push(turn.id.clone());
                    }
                }
                self.turns.push(turn.clone());
            }
            TurnType::Interjection => {
                turn.status = TurnStatus::Signal;
                self.turns.push(turn.clone());
            }
            TurnType::User => {
                if let Some(last) = self.turns.last_mut() {
                    if last.turn_type == TurnType::User && last.status == TurnStatus::Active {
                        last.text.push(' ');
                        last.text.push_str(&turn.text);
                        last.end_time = now;
                        return last.clone();
                    }
                }
                self.turns.push(turn.clone());
            }
            TurnType::Append | TurnType::Model => {
                self.turns.push(turn.clone());
            }
        }

        turn
    }

    pub fn reconcile(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for turn in &self.turns {
            match turn.status {
                TurnStatus::Active | TurnStatus::Signal => {
                    let text = turn.text.trim();
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                _ => {}
            }
        }
        parts.join(" ")
    }

    pub fn conversation_history(&self) -> Vec<serde_json::Value> {
        self.turns.iter()
            .filter(|t| matches!(t.turn_type, TurnType::User | TurnType::Model)
                && t.status != TurnStatus::Filtered)
            .map(|t| serde_json::json!({
                "role": if t.turn_type == TurnType::User { "user" } else { "assistant" },
                "content": t.text,
            }))
            .collect()
    }

    pub fn reset(&mut self) {
        self.turns.clear();
        self.counter = 0;
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }
}

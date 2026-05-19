use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "interjections", about = "Stream-of-thought voice interaction")]
pub struct Cli {
    #[arg(long, default_value = "false")]
    pub web: bool,

    #[arg(long, default_value_t = 8765)]
    pub port: u16,

    #[arg(long, default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long)]
    pub debug: bool,

    #[arg(long, env = "CARTESIA_API_KEY")]
    pub cartesia_key: Option<String>,

    #[arg(long)]
    pub sherpa_model_dir: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:4096")]
    pub opencode_url: String,

    #[arg(long, env = "IJ_GATE_ENDPOINT", default_value = "http://localhost:11434/v1/chat/completions")]
    pub gate_endpoint: String,

    #[arg(long, env = "IJ_GATE_MODEL", default_value = "qwen3.5:4b")]
    pub gate_model: String,

    #[arg(long, env = "IJ_GATE_API_KEY")]
    pub gate_api_key: Option<String>,

    #[arg(long, default_value_t = false)]
    pub no_gate: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub sample_rate: u32,
    pub channels: u16,
    pub chunk_ms: u32,
    pub vad_threshold: f32,
    pub vad_min_speech_ms: u32,
    pub vad_min_silence_ms: u32,
    pub sherpa_model_dir: String,
    pub sherpa_sample_rate: u32,
    pub cartesia_api_key: String,
    pub cartesia_model_id: String,
    pub cartesia_voice_id: String,
    pub cartesia_ws_url: String,
    pub silence_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub thinking_timeout_ms: u64,
    pub interjection_cues: Vec<String>,
    pub backchannel_cues: Vec<String>,
    pub correction_signals: Vec<String>,
    pub append_signals: Vec<String>,
    pub web_host: String,
    pub web_port: u16,
    pub debug: bool,
    pub tts_sample_rate: u32,
    pub opencode_server_url: String,
    pub opencode_username: String,
    pub opencode_password: String,
    pub gate_endpoint: String,
    pub gate_model: String,
    pub gate_api_key: Option<String>,
    pub gate_timeout_ms: u64,
    pub gate_silence_fallback_ms: u64,
    pub gate_max_rechecks: u32,
    pub gate_hold_backstop_ms: u64,
    pub gate_debounce_ms: u64,
    pub no_gate: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            chunk_ms: 30,
            vad_threshold: 0.5,
            vad_min_speech_ms: 100,
            vad_min_silence_ms: 300,
            sherpa_model_dir: "data/models/sherpa-zipformer-en".into(),
            sherpa_sample_rate: 16000,
            cartesia_api_key: String::new(),
            cartesia_model_id: "sonic-3".into(),
            cartesia_voice_id: "1463a4e1-56a1-4b41-b257-728d56e93605".into(),
            cartesia_ws_url: "wss://api.cartesia.ai/tts/websocket".into(),
            silence_timeout_ms: 600,
            idle_timeout_ms: 2000,
            thinking_timeout_ms: 15000,
            interjection_cues: vec![
                "wait".into(), "no".into(), "actually".into(), "hold on".into(),
                "nevermind".into(), "scratch that".into(), "cancel".into(),
                "i meant".into(), "correction".into(), "instead".into(),
            ],
            backchannel_cues: vec![
                "uh-huh".into(), "uh huh".into(), "mhm".into(), "yeah".into(),
                "right".into(), "okay".into(), "ok".into(), "sure".into(),
            ],
            correction_signals: vec![
                "actually".into(), "i meant".into(), "correction".into(),
                "no wait".into(), "scratch that".into(), "let me rephrase".into(),
            ],
            append_signals: vec![
                "also".into(), "and another thing".into(), "plus".into(), "one more".into(),
            ],
            web_host: "0.0.0.0".into(),
            web_port: 8765,
            debug: false,
            tts_sample_rate: 24000,
            opencode_server_url: "http://127.0.0.1:4096".into(),
            opencode_username: String::new(),
            opencode_password: String::new(),
            gate_endpoint: "http://localhost:11434/v1/chat/completions".into(),
            gate_model: "qwen3.5:4b".into(),
            gate_api_key: None,
            gate_timeout_ms: 4000,
            gate_silence_fallback_ms: 5000,
            gate_max_rechecks: 3,
            gate_hold_backstop_ms: 120000,
            gate_debounce_ms: 150,
            no_gate: false,
        }
    }
}

impl Config {
    pub fn from_cli(cli: &Cli) -> anyhow::Result<Self> {
        let mut cfg = Config::default();
        cfg.debug = cli.debug;
        cfg.web_host = cli.host.clone();
        cfg.web_port = cli.port;
        cfg.opencode_server_url = cli.opencode_url.clone();
        cfg.opencode_username = std::env::var("OPENCODE_USERNAME")
            .unwrap_or_else(|_| "opencode".to_string());
        cfg.opencode_password = std::env::var("OPENCODE_PASSWORD")
            .unwrap_or_default();
        if let Some(dir) = &cli.sherpa_model_dir {
            cfg.sherpa_model_dir = dir.clone();
        }
        cfg.cartesia_api_key = cli.cartesia_key.clone()
            .or_else(|| std::env::var("CARTESIA_API_KEY").ok())
            .unwrap_or_default();
        cfg.gate_endpoint = cli.gate_endpoint.clone();
        cfg.gate_model = cli.gate_model.clone();
        cfg.gate_api_key = cli.gate_api_key.clone();
        cfg.no_gate = cli.no_gate;
        Ok(cfg)
    }
}

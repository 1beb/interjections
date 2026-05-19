use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    Router, extract::State, extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
};
use base64::Engine;
use futures_util::StreamExt;
use hyper_util::service::TowerToHyperService;
use reqwest::Client;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use crate::Config;
use crate::controller::WebChannels;

fn cert_path() -> String {
    std::env::var("TLS_CERT_PATH").unwrap_or_else(|_| "certs/cert.pem".to_string())
}

fn key_path() -> String {
    std::env::var("TLS_KEY_PATH").unwrap_or_else(|_| "certs/key.pem".to_string())
}

#[derive(Clone)]
pub struct SharedState {
    pub audio_tx: broadcast::Sender<Vec<i16>>,
    pub state_tx: broadcast::Sender<String>,
    pub transcript_tx: broadcast::Sender<(String, String)>,
    pub log_tx: broadcast::Sender<String>,
    pub asr_audio_tx: tokio::sync::mpsc::Sender<crate::audio::AudioMessage>,
    pub config: Config,
    pub opencode_client: Client,
    pub auth_header: String,
}

pub async fn start(
    config: Config,
    channels: WebChannels,
    asr_audio_tx: tokio::sync::mpsc::Sender<crate::audio::AudioMessage>,
) -> anyhow::Result<()> {
    let auth = format!("{}:{}", config.opencode_username, config.opencode_password);
    let auth_header = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(auth.as_bytes()));

    let opencode_client = Client::builder().build()?;

    let shared = SharedState {
        audio_tx: channels.audio_tx,
        state_tx: channels.state_tx,
        transcript_tx: channels.transcript_tx,
        log_tx: channels.log_tx,
        asr_audio_tx,
        config: config.clone(),
        opencode_client,
        auth_header,
    };

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback(proxy_handler)
        .with_state(shared)
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::new(
        config.web_host.parse().unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        config.web_port,
    );

    log::info!("Web server at https://{}", addr);

    let certs = load_certs(&cert_path())?;
    let key = load_key(&key_path())?;

    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    loop {
        let (stream, peer) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let svc = TowerToHyperService::new(app.clone());

        tokio::spawn(async move {
            log::debug!("Connection from {}", peer);
            let tls_stream = match tls_acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    log::debug!("TLS handshake failed: {}", e);
                    return;
                }
            };
            let io = hyper_util::rt::TokioIo::new(tls_stream);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .with_upgrades()
                .await
            {
                log::debug!("Connection error: {}", e);
            }
        });
    }
}

async fn proxy_handler(
    State(state): State<SharedState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let target_url = format!("http://127.0.0.1:4096{}{}", path, query);

    let method = req.method().clone();
    let headers = req.headers().clone();

    let (_, body) = req.into_parts();
    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(_) => Vec::new(),
    };

    let mut proxy_req = state.opencode_client.request(method, &target_url);

    for (key, val) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if key_str != "host" && key_str != "content-length" && key_str != "content-encoding"
            && key_str != "accept-encoding"
        {
            proxy_req = proxy_req.header(key, val);
        }
    }
    proxy_req = proxy_req.header("Authorization", &state.auth_header);

    if !body_bytes.is_empty() {
        proxy_req = proxy_req.body(body_bytes);
    }

    match proxy_req.send().await {
        Ok(resp) => {
            let is_html = resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("text/html"))
                .unwrap_or(false)
                && resp.status().is_success();

            if is_html {
                match resp.text().await {
                    Ok(html) => {
                        let modified = if let Some(pos) = html.rfind("</body>") {
                            format!("{}{}{}", &html[..pos], VOICE_WIDGET, &html[pos + 7..])
                        } else if let Some(pos) = html.rfind("</html>") {
                            format!("{} {} </html>", &html[..pos], VOICE_WIDGET)
                        } else {
                            format!("{} {}", html, VOICE_WIDGET)
                        };
                        axum::response::Response::builder()
                            .status(200)
                            .header("content-type", "text/html; charset=utf-8")
                            .body(axum::body::Body::from(modified))
                            .unwrap_or_else(|_| resp_500())
                    }
                    Err(_) => resp_500(),
                }
            } else {
                let status = resp.status();
                let mut response_builder = axum::response::Response::builder().status(status);
                for (key, val) in resp.headers().iter() {
                    let key_str = key.as_str().to_lowercase();
                    if key_str != "content-encoding" && key_str != "transfer-encoding" && key_str != "content-length" {
                        if let Ok(name) = axum::http::HeaderName::from_bytes(key.as_str().as_bytes()) {
                            response_builder = response_builder.header(name, val);
                        }
                    }
                }
                let stream = resp.bytes_stream();
                let body = axum::body::Body::from_stream(stream.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))));
                response_builder
                    .body(body)
                    .unwrap_or_else(|_| resp_500())
            }
        }
        Err(e) => {
            log::error!("Proxy error for {}: {}", path, e);
            axum::response::Response::builder()
                .status(502)
                .header("content-type", "text/plain")
                .body(axum::body::Body::from(format!("Proxy error: {}", e)))
                .unwrap()
        }
    }
}

fn resp_500() -> axum::response::Response {
    axum::response::Response::builder()
        .status(500)
        .body(axum::body::Body::empty())
        .unwrap()
}

fn auth_header(config: &crate::Config) -> String {
    let auth = format!("{}:{}", config.opencode_username, config.opencode_password);
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(auth.as_bytes()))
}

async fn listen_for_response(
    session_id: &str,
    auth_header: &str,
    tts_tx: tokio::sync::broadcast::Sender<Vec<i16>>,
    config: &crate::Config,
) {
    use futures_util::StreamExt;
    use reqwest::Client;

    let client = Client::new();
    let sse_url = "http://127.0.0.1:4096/global/event";

    log::info!("SSE listener starting for session {}", session_id);
    let resp = match client.get(sse_url)
        .header("Authorization", auth_header)
        .send().await
    {
        Ok(r) => {
            log::info!("SSE connected, status={}", r.status());
            r
        }
        Err(e) => {
            log::error!("SSE connect failed: {}", e);
            return;
        }
    };

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut response_text = String::new();
    let mut part_types: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let session = session_id.to_string();
    let mut event_count = 0u64;

    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else { break };
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buf.find("\n\n") {
            let block = buf[..pos].to_string();
            buf = buf[pos + 2..].to_string();

            let mut event_type = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") { event_type = v.trim().to_string(); }
                else if let Some(v) = line.strip_prefix("data: ") { data.push_str(v.trim()); }
            }

            let _ = event_type;
            if data.is_empty() { continue; }

            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            if value.get("payload").is_some() { value = value["payload"].clone(); }

            let evt_type = value["type"].as_str().unwrap_or("");
            let props = &value["properties"];
            let sid = props["sessionID"].as_str().unwrap_or("");

            if sid != session { continue; }

            event_count += 1;

            match evt_type {
                "message.part.updated" => {
                    let part = &props["part"];
                    if let (Some(pid), Some(ptype)) = (part["id"].as_str(), part["type"].as_str()) {
                        part_types.insert(pid.to_string(), ptype.to_string());
                    }
                }
                "message.part.delta" => {
                    if props["field"].as_str() == Some("text") {
                        let pid = props["partID"].as_str().unwrap_or("");
                        // Only accumulate deltas for text parts — reasoning parts
                        // also emit field="text" deltas (processor.ts:246) but we
                        // don't want to TTS thinking tokens.
                        if part_types.get(pid).map(|s| s.as_str()) == Some("text") {
                            if let Some(delta) = props["delta"].as_str() {
                                response_text.push_str(delta);
                            }
                        }
                    }
                }
                "message.updated" => {
                    let role = props["info"]["role"].as_str().unwrap_or("");
                    let finish = props["info"]["finish"].as_str();
                    if role == "assistant" && finish.is_some() {
                        log::info!("assistant finish={:?} accumulated={} chars", finish, response_text.len());
                    }
                    if role == "assistant" && finish.is_some() && !response_text.trim().is_empty() {
                        log::info!("Response complete ({} events, {} chars), starting TTS", event_count, response_text.len());
                        let tts = crate::tts::CartesiaTts::new(config.clone());
                        let tts_tx_clone = tts_tx.clone();
                        let _ = tts.speak(
                            &response_text,
                            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            move |samples| { let _ = tts_tx_clone.send(samples); },
                            || {},
                        ).await;
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

fn load_certs(path: &str) -> anyhow::Result<Vec<rustls_pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(certs)
}

fn load_key(path: &str) -> anyhow::Result<rustls_pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let keys = std::iter::from_fn(|| {
        rustls_pemfile::private_key(&mut reader).transpose()
    })
    .collect::<Result<Vec<_>, _>>()?;
    keys.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("No private key found in {}", path))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    req: axum::extract::Request,
) -> impl axum::response::IntoResponse {
    let config_clone = state.config.clone();
    let session = req.uri().query()
        .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("session=")))
        .unwrap_or("")
        .to_string();
    ws.on_upgrade(move |socket| {
        log::info!("Voice widget connecting with session='{}'", session);
        let audio_rx = state.audio_tx.subscribe();
        let audio_tx = state.audio_tx.clone();
        let state_rx = state.state_tx.subscribe();
        let transcript_rx = state.transcript_tx.subscribe();
        let log_rx = state.log_tx.subscribe();
        handle_ws(socket, audio_rx, audio_tx, state_rx, transcript_rx, log_rx, state.asr_audio_tx, config_clone, session)
    })
}

async fn handle_ws(
    mut ws: WebSocket,
    mut audio_rx: broadcast::Receiver<Vec<i16>>,
    audio_tx: broadcast::Sender<Vec<i16>>,
    mut state_rx: broadcast::Receiver<String>,
    mut transcript_rx: broadcast::Receiver<(String, String)>,
    mut log_rx: broadcast::Receiver<String>,
    asr_audio_tx: tokio::sync::mpsc::Sender<crate::audio::AudioMessage>,
    config: crate::Config,
    active_session: String,
) {
    log::info!("Voice widget connected");

    let mut vad = crate::vad::EnergyVad::new(
        config.vad_threshold,
        config.vad_min_speech_ms,
        config.vad_min_silence_ms,
        config.chunk_ms,
    );
    let mut noise_suppressor = crate::audio::NoiseSuppressor::new();

    loop {
        tokio::select! {
            msg = ws.recv() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let raw_samples: Vec<i16> = data
                            .chunks(2)
                            .filter(|c| c.len() == 2)
                            .map(|c| i16::from_ne_bytes([c[0], c[1]]))
                            .collect();
                        if !raw_samples.is_empty() {
                            let clean = noise_suppressor.process(&raw_samples);
                            let vad_speaking = vad.process(&clean);
                            let float_samples: Vec<f32> = clean.iter()
                                .map(|&s| s as f32 / 32768.0)
                                .collect();
                            let _ = asr_audio_tx.try_send(crate::audio::AudioMessage::Chunk {
                                samples: float_samples,
                                vad_speaking,
                            });
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            result = audio_rx.recv() => {
                if let Ok(samples) = result {
                    let bytes: axum::body::Bytes = samples.iter()
                        .flat_map(|s| s.to_ne_bytes())
                        .collect();
                    if ws.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
            }
            result = state_rx.recv() => {
                if let Ok(state_str) = result {
                    let msg = serde_json::json!({"type": "state", "state": state_str}).to_string();
                    if ws.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
            }
            result = transcript_rx.recv() => {
                if let Ok((text, kind)) = result {
                    let msg = serde_json::json!({"type": kind, "text": text}).to_string();
                    if ws.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                    if kind == "submit" {
                        log::info!("submit broadcast, session='{}' len={}", active_session, active_session.len());
                        if !active_session.is_empty() {
                            let session = active_session.clone();
                            let tts_tx = audio_tx.clone();
                            let cfg = config.clone();
                            let auth = auth_header(&cfg);
                            tokio::spawn(async move {
                                listen_for_response(&session, &auth, tts_tx, &cfg).await;
                            });
                        }
                    }
                }
            }
            result = log_rx.recv() => {
                if let Ok(log_msg) = result {
                    let msg = serde_json::json!({"type": "log", "text": log_msg}).to_string();
                    if ws.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    log::info!("Voice widget disconnected");
}

const VOICE_WIDGET: &str = r#"<div id="ij-voice-widget" style="position:fixed;bottom:24px;right:24px;z-index:9999;display:flex;flex-direction:column;align-items:center;gap:8px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif">
<style>
  #ij-mic-btn { width:56px;height:56px;border-radius:50%;border:none;cursor:pointer;font-size:24px;
    background:#1a1a1a;color:#888;box-shadow:0 2px 12px rgba(0,0,0,0.3);
    transition:all 0.15s;display:flex;align-items:center;justify-content:center; }
  #ij-mic-btn:hover { background:#2a2a2a;color:#e0e0e0; }
  #ij-mic-btn.listening { background:#3b82f6;color:#fff;box-shadow:0 0 20px rgba(59,130,246,0.4); }
  #ij-mic-btn.thinking { background:#8b5cf6;color:#fff;animation:ij-pulse 0.8s infinite; }
  #ij-mic-btn.speaking { background:#22c55e;color:#fff;box-shadow:0 0 20px rgba(34,197,94,0.4); }
  @keyframes ij-pulse { 0%,100%{opacity:1} 50%{opacity:0.4} }
  #ij-status { font-size:11px;color:#666;text-transform:uppercase;letter-spacing:1px; }
  #ij-transcript { font-size:12px;color:#999;max-width:280px;text-align:center;overflow:hidden;
    text-overflow:ellipsis;white-space:nowrap;background:#111;padding:4px 12px;border-radius:8px; }
</style>
<button id="ij-mic-btn" onclick="ijToggle()">🎤</button>
<div id="ij-status">disconnected</div>
<div id="ij-transcript" style="display:none"></div>
</div>
<script>
let ijWs=null,ijMediaStream=null,ijAudioCtx=null,ijScriptNode=null,ijSourceNode=null;
let ijIsMuted=false,ijNextPlayTime=0,ijIsRecording=false;
const IJ_RATE=16000,IJ_PLAYBACK_RATE=24000;
function ijLog(m){let d=document.getElementById('ij-transcript');d.textContent=m;d.style.display=m?'block':'none'}
function ijSetState(s){
  let b=document.getElementById('ij-mic-btn'),st=document.getElementById('ij-status');
  b.className='';
  if(s==='idle'||s==='connected'){b.classList.add('connected');st.textContent='connected'}
  else if(s==='user'||s==='listening'){b.classList.add('listening');st.textContent='listening...'}
  else if(s==='checking'){b.classList.add('listening');st.textContent='checking...'}
  else if(s==='gating'||s==='go-on'){b.classList.add('listening');st.textContent='go on...'}
  else if(s==='model'||s==='speaking'){b.classList.add('speaking');st.textContent='speaking'}
  else if(s==='thinking'){b.classList.add('thinking');st.textContent='thinking...'}
  else{st.textContent=s||'disconnected'}
}
function ijPlayPcm(d){
  if(!ijAudioCtx)return;
  let f32=new Float32Array(d.length);
  for(let i=0;i<d.length;i++)f32[i]=d[i]/32768;
  let b=ijAudioCtx.createBuffer(1,f32.length,IJ_PLAYBACK_RATE);
  b.getChannelData(0).set(f32);
  let s=ijAudioCtx.createBufferSource();s.buffer=b;s.connect(ijAudioCtx.destination);
  let n=ijAudioCtx.currentTime;
  if(ijNextPlayTime<n)ijNextPlayTime=n+0.05;
  s.start(ijNextPlayTime);ijNextPlayTime+=b.duration;
}
function ijDownsample(e){
  if(!ijWs||ijWs.readyState!==WebSocket.OPEN)return;
  let d=e.inputBuffer.getChannelData(0),r=ijAudioCtx.sampleRate;
  if(r===IJ_RATE){let i16=new Int16Array(d.length);for(let i=0;i<d.length;i++)i16[i]=Math.max(-32768,Math.min(32767,Math.round(d[i]*32767)));ijWs.send(i16.buffer)}
  else{let ratio=r/IJ_RATE,outLen=Math.round(d.length/ratio),i16=new Int16Array(outLen);for(let i=0;i<outLen;i++){let si=Math.round(i*ratio),s=d[Math.min(si,d.length-1)];i16[i]=Math.max(-32768,Math.min(32767,Math.round(s*32767)))};ijWs.send(i16.buffer)}
}
function ijToggle(){
  let b=document.getElementById('ij-mic-btn');
  if(ijIsRecording){ijStopRecording();ijWs&&ijWs.close();ijIsRecording=false;b.classList.remove('listening');document.getElementById('ij-status').textContent='disconnected'}
  else{ijStart()}
}
function ijInjectText(text){
  let editor=document.querySelector('[data-component="prompt-input"]')||document.querySelector('[contenteditable="true"]')||document.querySelector('[role="textbox"]');
  if(!editor){ijLog('no editor');return}
  ijLog('inject:'+text.slice(0,40));
  editor.focus();
  // Select all existing content so insertText replaces it
  let sel=window.getSelection();
  let range=document.createRange();
  range.selectNodeContents(editor);
  sel.removeAllRanges();sel.addRange(range);
  // execCommand fires real beforeinput+input events SolidJS will react to
  let ok=document.execCommand('insertText',false,text);
  if(!ok){
    editor.textContent=text;
    editor.dispatchEvent(new InputEvent('input',{inputType:'insertText',data:text,bubbles:true,cancelable:true}));
  }
  // Click the real submit button — triggers handleSubmit via the form
  setTimeout(()=>{
    let btn=document.querySelector('[data-action="prompt-submit"]')||editor.closest('form')?.querySelector('button[type="submit"]');
    ijLog('submit:'+(btn?'ok':'none'));
    if(btn)btn.click();
  },50);
}
async function ijStart(){
  try{
    ijMediaStream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1,sampleRate:{ideal:IJ_RATE},echoCancellation:true,noiseSuppression:true}});
    let p=location.protocol==='https:'?'wss:':'ws:';
    let s=window.location.pathname.match(/\/session\/([^/]+)/);
    ijWs=new WebSocket(p+'//'+location.host+'/ws?session='+(s?s[1]:''));ijWs.binaryType='arraybuffer';
    ijWs.onopen=()=>{ijSetState('connected');ijStartRecording();ijIsRecording=true};
    ijWs.onmessage=(e)=>{
      if(e.data instanceof ArrayBuffer){ijPlayPcm(new Int16Array(e.data))}
      else{try{let m=JSON.parse(e.data);
        if(m.type==='state')ijSetState(m.state);
        else if(m.type==='submit'){ijInjectText(m.text);ijLog('')}
        else if(m.type==='log')ijLog(m.text)
      }catch(e){}}
    };
    ijWs.onclose=()=>{ijStopRecording();ijIsRecording=false;document.getElementById('ij-status').textContent='disconnected';ijWs=null};
  }catch(e){ijLog('mic error: '+e.message)}
}
function ijStartRecording(){
  if(!ijMediaStream||!ijWs||ijWs.readyState!==WebSocket.OPEN||ijSourceNode)return;
  ijAudioCtx=new(window.AudioContext||window.webkitAudioContext)();
  ijSourceNode=ijAudioCtx.createMediaStreamSource(ijMediaStream);
  ijScriptNode=ijAudioCtx.createScriptProcessor(4096,1,1);
  ijScriptNode.onaudioprocess=ijDownsample;
  ijSourceNode.connect(ijScriptNode);ijScriptNode.connect(ijAudioCtx.destination);
}
function ijStopRecording(){
  if(ijScriptNode){ijScriptNode.disconnect();ijScriptNode=null}
  if(ijSourceNode){ijSourceNode.disconnect();ijSourceNode=null}
  if(ijMediaStream){ijMediaStream.getTracks().forEach(t=>t.stop());ijMediaStream=null}
  if(ijAudioCtx){ijAudioCtx.close();ijAudioCtx=null}
}
</script>"#;

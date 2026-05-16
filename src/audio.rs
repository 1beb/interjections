use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[derive(Clone)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub vad_speaking: bool,
}

#[derive(Clone)]
pub enum AudioMessage {
    Chunk { samples: Vec<f32>, vad_speaking: bool },
}

pub struct AudioCapture {
    _stream: cpal::Stream,
}

impl AudioCapture {
    pub fn new(
        _sample_rate: u32,
        _chunk_ms: u32,
        mut on_audio: impl FnMut(Vec<i16>, u32) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))?;
        let config = device.default_input_config()?;
        let channels = config.channels() as usize;
        let actual_rate = config.sample_rate().0;
        let err_fn = move |err| log::error!("Audio error: {}", err);

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut samples = Vec::with_capacity(data.len() / channels);
                for frame in data.chunks(channels) {
                    samples.push((frame[0] * 32767.0) as i16);
                }
                on_audio(samples, actual_rate);
            },
            err_fn,
            None,
        )?;

        stream.play()?;
        Ok(Self { _stream: stream })
    }
}

pub struct AudioOutput {
    _stream: cpal::Stream,
}

impl AudioOutput {
    pub fn new(_sample_rate: u32) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No output device found"))?;
        let config = device.default_output_config()?;
        let err_fn = move |err| log::error!("Audio output error: {}", err);
        let channels = config.channels() as usize;

        // Queue-based output: store incoming samples and play from the callback
        let queue = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i16>::new()));

        let queue_clone = queue.clone();
        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut q = queue_clone.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let sample = if q.len() >= 2 { q.remove(0) } else { 0i16 };
                    let val = sample as f32 / 32768.0;
                    for ch in frame.iter_mut() {
                        *ch = val;
                    }
                }
            },
            err_fn,
            None,
        )?;

        stream.play()?;
        Ok(Self { _stream: stream })
    }

    pub fn play(&self, _samples: Vec<i16>) {
        // For now, TTS audio is only played via web broadcast
        // Terminal playback requires a proper ring buffer
        log::warn!("Terminal TTS playback not implemented yet");
    }
}

pub struct NoiseSuppressor {
    state: Box<nnnoiseless::DenoiseState<'static>>,
}

impl NoiseSuppressor {
    pub fn new() -> Self {
        Self { state: nnnoiseless::DenoiseState::new() }
    }

    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        let mut output = Vec::with_capacity(input.len());
        for chunk in input.chunks(480) {
            let chunk_len = chunk.len();
            let (float_in, extra) = if chunk_len < 480 {
                let mut padded = [0i16; 480];
                padded[..chunk_len].copy_from_slice(chunk);
                (padded.iter().map(|&s| s as f32 / 32768.0).collect::<Vec<_>>(), true)
            } else {
                (chunk.iter().map(|&s| s as f32 / 32768.0).collect(), false)
            };
            let mut out = [0f32; 480];
            self.state.process_frame(&mut out, &float_in);
            let n = if extra { chunk_len } else { 480 };
            for &s in out.iter().take(n) {
                output.push((s * 32767.0) as i16);
            }
        }
        output
    }
}

pub fn resample(input: &[i16], rate_in: u32, rate_out: u32) -> Vec<i16> {
    if rate_in == rate_out {
        return input.to_vec();
    }
    let ratio = rate_out as f64 / rate_in as f64;
    let output_len = (input.len() as f64 * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let src_pos = i as f64 / ratio;
        let src_idx = src_pos as usize;
        let frac = (src_pos - src_idx as f64) as f32;
        let a = *input.get(src_idx).unwrap_or(&0) as f32;
        let b = *input.get(src_idx + 1).unwrap_or(&0) as f32;
        output.push((a + (b - a) * frac) as i16);
    }
    output
}

// Native microphone capture using cpal — bypasses WebKit/browser entirely
// so macOS does NOT interfere with Zoom/Teams/Meet mic access.
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tracing::{error, warn};

/// State for mic capture — managed by Tauri
pub struct MicState {
    is_capturing: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    stream_handle: Arc<Mutex<Option<cpal::Stream>>>,
}

impl Default for MicState {
    fn default() -> Self {
        Self {
            is_capturing: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            stream_handle: Arc::new(Mutex::new(None)),
        }
    }
}

/// List available input (microphone) devices
#[tauri::command]
pub fn list_mic_devices() -> Result<Vec<MicDeviceInfo>, String> {
    let host = cpal::default_host();
    let mut devices = Vec::new();

    if let Some(default) = host.default_input_device() {
        let name = default.name().unwrap_or_else(|_| "Default".to_string());
        devices.push(MicDeviceInfo {
            id: "default".to_string(),
            name,
            is_default: true,
        });
    }

    if let Ok(input_devices) = host.input_devices() {
        for device in input_devices {
            let name = device.name().unwrap_or_else(|_| "Unknown".to_string());
            // Skip if it's the same as default (already added)
            if !devices.iter().any(|d| d.name == name) {
                devices.push(MicDeviceInfo {
                    id: name.clone(),
                    name,
                    is_default: false,
                });
            }
        }
    }

    Ok(devices)
}

#[derive(serde::Serialize, Clone)]
pub struct MicDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Start capturing mic audio and emit chunks to the frontend.
/// Audio is captured natively via CoreAudio (cpal) — no browser/WebKit involvement.
#[tauri::command]
pub fn start_mic_capture(
    app: AppHandle,
    device_name: Option<String>,
) -> Result<u32, String> {
    let state = app.state::<MicState>();

    // Prevent double-capture
    if state.is_capturing.load(Ordering::SeqCst) {
        return Err("Mic capture already running".to_string());
    }

    let host = cpal::default_host();

    // Find the requested device (or use default)
    let device = if let Some(ref name) = device_name {
        if name == "default" {
            host.default_input_device()
        } else {
            host.input_devices()
                .ok()
                .and_then(|mut devices| devices.find(|d| d.name().ok().as_deref() == Some(name)))
                .or_else(|| host.default_input_device())
        }
    } else {
        host.default_input_device()
    }
    .ok_or_else(|| "No input device available".to_string())?;

    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to get input config: {}", e))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    // Reset flags
    state.stop_flag.store(false, Ordering::SeqCst);
    state.is_capturing.store(true, Ordering::SeqCst);

    let stop_flag = state.stop_flag.clone();
    let is_capturing = state.is_capturing.clone();
    let app_clone = app.clone();

    // VAD state for real-time speech detection
    let vad_state = Arc::new(Mutex::new(VadState::new(sample_rate)));
    let vad_for_callback = vad_state.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_input_stream::<f32>(
                &device,
                &config.into(),
                channels,
                stop_flag,
                is_capturing,
                app_clone,
                vad_for_callback,
            )
        }
        cpal::SampleFormat::I16 => {
            build_input_stream::<i16>(
                &device,
                &config.into(),
                channels,
                stop_flag,
                is_capturing,
                app_clone,
                vad_for_callback,
            )
        }
        cpal::SampleFormat::U16 => {
            build_input_stream::<u16>(
                &device,
                &config.into(),
                channels,
                stop_flag,
                is_capturing,
                app_clone,
                vad_for_callback,
            )
        }
        _ => Err("Unsupported sample format".to_string()),
    }?;

    stream.play().map_err(|e| format!("Failed to start mic stream: {}", e))?;

    // Store stream handle so it stays alive
    *state.stream_handle.lock().unwrap() = Some(stream);

    let _ = app.emit("mic-capture-started", sample_rate);

    Ok(sample_rate)
}

/// Stop mic capture
#[tauri::command]
pub fn stop_mic_capture(app: AppHandle) -> Result<(), String> {
    let state = app.state::<MicState>();

    state.stop_flag.store(true, Ordering::SeqCst);
    state.is_capturing.store(false, Ordering::SeqCst);

    // Drop the stream to release the mic immediately
    if let Ok(mut handle) = state.stream_handle.lock() {
        *handle = None;
    }

    let _ = app.emit("mic-capture-stopped", ());

    Ok(())
}

/// Check if mic capture is active
#[tauri::command]
pub fn is_mic_capturing(app: AppHandle) -> Result<bool, String> {
    let state = app.state::<MicState>();
    Ok(state.is_capturing.load(Ordering::SeqCst))
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// VAD state for detecting speech in mic audio
struct VadState {
    sample_rate: u32,
    buffer: Vec<f32>,
    pre_speech_buffer: Vec<f32>,
    speech_buffer: Vec<f32>,
    in_speech: bool,
    silence_count: usize,
    speech_count: usize,
    // Tuned for mic input (closer to mouth = louder signal)
    hop_size: usize,
    rms_threshold: f32,
    peak_threshold: f32,
    silence_chunks_needed: usize,
    min_speech_chunks: usize,
    pre_speech_samples: usize,
}

impl VadState {
    fn new(sample_rate: u32) -> Self {
        let hop_size = 1024;
        Self {
            sample_rate,
            buffer: Vec::new(),
            pre_speech_buffer: Vec::with_capacity(hop_size * 12),
            speech_buffer: Vec::new(),
            in_speech: false,
            silence_count: 0,
            speech_count: 0,
            hop_size,
            rms_threshold: 0.015,
            peak_threshold: 0.04,
            silence_chunks_needed: 40, // ~0.9s silence to end
            min_speech_chunks: 5,      // ~0.12s minimum speech
            pre_speech_samples: hop_size * 10,
        }
    }

    /// Feed mono f32 samples, returns completed speech segments as WAV base64
    fn feed(&mut self, mono_samples: &[f32]) -> Vec<String> {
        let mut results = Vec::new();
        self.buffer.extend_from_slice(mono_samples);

        while self.buffer.len() >= self.hop_size {
            let chunk: Vec<f32> = self.buffer.drain(..self.hop_size).collect();

            // Calculate RMS and peak
            let mut sumsq = 0.0f32;
            let mut peak = 0.0f32;
            for &v in &chunk {
                let a = v.abs();
                peak = peak.max(a);
                sumsq += v * v;
            }
            let rms = (sumsq / chunk.len() as f32).sqrt();
            let is_speech = rms > self.rms_threshold || peak > self.peak_threshold;

            if is_speech {
                if !self.in_speech {
                    self.in_speech = true;
                    self.speech_count = 0;
                    // Include pre-speech buffer
                    self.speech_buffer.clear();
                    self.speech_buffer.extend_from_slice(&self.pre_speech_buffer);
                }
                self.speech_count += 1;
                self.speech_buffer.extend_from_slice(&chunk);
                self.silence_count = 0;

                // Safety cap: 30s per utterance
                let max_samples = self.sample_rate as usize * 30;
                if self.speech_buffer.len() > max_samples {
                    if let Ok(b64) = samples_to_wav_b64(self.sample_rate, &self.speech_buffer) {
                        results.push(b64);
                    }
                    self.speech_buffer.clear();
                    self.in_speech = false;
                    self.speech_count = 0;
                }
            } else if self.in_speech {
                self.silence_count += 1;
                self.speech_buffer.extend_from_slice(&chunk);

                if self.silence_count >= self.silence_chunks_needed {
                    if self.speech_count >= self.min_speech_chunks && !self.speech_buffer.is_empty() {
                        // Trim trailing silence (keep ~0.15s)
                        let silence_samples = self.silence_count * self.hop_size;
                        let keep = (self.sample_rate as usize) * 15 / 100;
                        let trim = silence_samples.saturating_sub(keep);
                        if self.speech_buffer.len() > trim {
                            self.speech_buffer.truncate(self.speech_buffer.len() - trim);
                        }

                        if let Ok(b64) = samples_to_wav_b64(self.sample_rate, &self.speech_buffer) {
                            results.push(b64);
                        }
                    }
                    self.speech_buffer.clear();
                    self.in_speech = false;
                    self.silence_count = 0;
                    self.speech_count = 0;
                }
            } else {
                // Not in speech — maintain rolling pre-speech buffer
                self.pre_speech_buffer.extend_from_slice(&chunk);
                if self.pre_speech_buffer.len() > self.pre_speech_samples {
                    let excess = self.pre_speech_buffer.len() - self.pre_speech_samples;
                    self.pre_speech_buffer.drain(..excess);
                }
            }
        }

        results
    }
}

fn build_input_stream<T: cpal::Sample + cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    stop_flag: Arc<AtomicBool>,
    _is_capturing: Arc<AtomicBool>,
    app: AppHandle,
    vad_state: Arc<Mutex<VadState>>,
) -> Result<cpal::Stream, String>
where
    f32: cpal::FromSample<T>,
{
    let stream = device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                if stop_flag.load(Ordering::Relaxed) {
                    return;
                }

                // Convert to mono f32
                let mono: Vec<f32> = if channels == 1 {
                    data.iter().map(|s| <f32 as cpal::FromSample<T>>::from_sample(*s)).collect()
                } else {
                    data.chunks(channels)
                        .map(|frame| {
                            let sum: f32 = frame.iter().map(|s| <f32 as cpal::FromSample<T>>::from_sample(*s)).sum();
                            sum / channels as f32
                        })
                        .collect()
                };

                // Feed to VAD
                if let Ok(mut vad) = vad_state.lock() {
                    let segments = vad.feed(&mono);
                    for b64 in segments {
                        let _ = app.emit("mic-speech-detected", &b64);
                    }
                }
            },
            move |err| {
                error!("Mic input stream error: {}", err);
            },
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {}", e))?;

    Ok(stream)
}

fn samples_to_wav_b64(sample_rate: u32, mono_f32: &[f32]) -> Result<String, String> {
    if mono_f32.is_empty() {
        return Err("Empty audio buffer".to_string());
    }

    let mut cursor = Cursor::new(Vec::new());
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::new(&mut cursor, spec).map_err(|e| e.to_string())?;

    for &s in mono_f32 {
        let clamped = s.clamp(-1.0, 1.0);
        let sample_i16 = (clamped * i16::MAX as f32) as i16;
        writer.write_sample(sample_i16).map_err(|e| e.to_string())?;
    }

    writer.finalize().map_err(|e| e.to_string())?;
    Ok(B64.encode(cursor.into_inner()))
}

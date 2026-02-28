// Native microphone capture using cpal — bypasses WebKit/browser entirely
// so macOS does NOT interfere with Zoom/Teams/Meet mic access.
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tracing::error;

/// State for mic capture — only contains Send+Sync types.
/// The cpal::Stream lives on a dedicated thread (not stored here).
pub struct MicState {
    pub is_capturing: Arc<AtomicBool>,
    pub stop_flag: Arc<AtomicBool>,
    /// Handle to the dedicated capture thread (so we can join on stop)
    pub thread_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Default for MicState {
    fn default() -> Self {
        Self {
            is_capturing: Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            thread_handle: Mutex::new(None),
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

/// Start capturing mic audio and emit speech events to the frontend.
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

    // We need to probe sample rate on the current thread first
    let host = cpal::default_host();
    let device = find_device(&host, &device_name)?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to get input config: {}", e))?;
    let sample_rate = config.sample_rate().0;

    // Reset flags
    state.stop_flag.store(false, Ordering::SeqCst);
    state.is_capturing.store(true, Ordering::SeqCst);

    let stop_flag = state.is_capturing.clone(); // alias for the thread
    let stop_signal = state.stop_flag.clone();
    let app_clone = app.clone();
    let device_name_clone = device_name.clone();

    // Spawn a dedicated thread that owns the cpal::Stream
    // (cpal::Stream is !Send on macOS, so it must stay on the thread that created it)
    let handle = std::thread::spawn(move || {
        run_mic_capture_thread(app_clone, device_name_clone, stop_signal);
    });

    // Store thread handle
    if let Ok(mut th) = state.thread_handle.lock() {
        *th = Some(handle);
    }

    let _ = app.emit("mic-capture-started", sample_rate);
    Ok(sample_rate)
}

/// Stop mic capture
#[tauri::command]
pub fn stop_mic_capture(app: AppHandle) -> Result<(), String> {
    let state = app.state::<MicState>();

    state.stop_flag.store(true, Ordering::SeqCst);
    state.is_capturing.store(false, Ordering::SeqCst);

    // Wait for thread to finish (drops the cpal::Stream, releasing the mic)
    if let Ok(mut th) = state.thread_handle.lock() {
        if let Some(handle) = th.take() {
            let _ = handle.join();
        }
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

// ─── Internal: capture thread ────────────────────────────────────────────────

fn find_device(host: &cpal::Host, device_name: &Option<String>) -> Result<cpal::Device, String> {
    if let Some(ref name) = device_name {
        if name != "default" {
            if let Some(dev) = host
                .input_devices()
                .ok()
                .and_then(|mut devs| devs.find(|d| d.name().ok().as_deref() == Some(name)))
            {
                return Ok(dev);
            }
        }
    }
    host.default_input_device()
        .ok_or_else(|| "No input device available".to_string())
}

/// Runs on a dedicated thread. Creates the cpal stream, processes audio,
/// and blocks until stop_flag is set. When it returns, the stream is dropped.
fn run_mic_capture_thread(
    app: AppHandle,
    device_name: Option<String>,
    stop_flag: Arc<AtomicBool>,
) {
    let host = cpal::default_host();

    let device = match find_device(&host, &device_name) {
        Ok(d) => d,
        Err(e) => {
            error!("Mic thread: failed to find device: {}", e);
            return;
        }
    };

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            error!("Mic thread: failed to get config: {}", e);
            return;
        }
    };

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let vad_state = Arc::new(Mutex::new(VadState::new(sample_rate)));
    let vad_for_callback = vad_state.clone();
    let stop_for_callback = stop_flag.clone();
    let app_for_callback = app.clone();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_input_stream::<f32>(
            &device, &config.into(), channels, stop_for_callback, app_for_callback, vad_for_callback,
        ),
        cpal::SampleFormat::I16 => build_input_stream::<i16>(
            &device, &config.into(), channels, stop_for_callback, app_for_callback, vad_for_callback,
        ),
        cpal::SampleFormat::U16 => build_input_stream::<u16>(
            &device, &config.into(), channels, stop_for_callback, app_for_callback, vad_for_callback,
        ),
        _ => {
            error!("Mic thread: unsupported sample format");
            return;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            error!("Mic thread: failed to build stream: {}", e);
            return;
        }
    };

    if let Err(e) = stream.play() {
        error!("Mic thread: failed to play stream: {}", e);
        return;
    }

    // Block this thread until stop is signaled.
    // The stream stays alive (and capturing) as long as we're here.
    while !stop_flag.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // stream is dropped here → mic is released
    drop(stream);
}

// ─── VAD ─────────────────────────────────────────────────────────────────────

struct VadState {
    sample_rate: u32,
    buffer: Vec<f32>,
    pre_speech_buffer: Vec<f32>,
    speech_buffer: Vec<f32>,
    in_speech: bool,
    silence_count: usize,
    speech_count: usize,
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
            silence_chunks_needed: 40,
            min_speech_chunks: 5,
            pre_speech_samples: hop_size * 10,
        }
    }

    fn feed(&mut self, mono_samples: &[f32]) -> Vec<String> {
        let mut results = Vec::new();
        self.buffer.extend_from_slice(mono_samples);

        while self.buffer.len() >= self.hop_size {
            let chunk: Vec<f32> = self.buffer.drain(..self.hop_size).collect();

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
                    self.speech_buffer.clear();
                    self.speech_buffer.extend_from_slice(&self.pre_speech_buffer);
                }
                self.speech_count += 1;
                self.speech_buffer.extend_from_slice(&chunk);
                self.silence_count = 0;

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

// ─── Stream builder ──────────────────────────────────────────────────────────

fn build_input_stream<T: cpal::Sample + cpal::SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    stop_flag: Arc<AtomicBool>,
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
                    data.iter()
                        .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                        .collect()
                } else {
                    data.chunks(channels)
                        .map(|frame| {
                            let sum: f32 = frame
                                .iter()
                                .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                                .sum();
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

// ─── WAV encoding ────────────────────────────────────────────────────────────

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

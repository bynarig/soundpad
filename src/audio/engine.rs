//! Owns the cpal streams (main thread only) and wires them to mixers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedBufferSize};
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

use super::mixer::{MicInput, Mixer, MixerHandle, OutputStatus, mixer};
use super::player::{OutputKind, Player};

/// Requested callback size. 256 frames ≈ 5 ms at 48 kHz.
pub const PREFERRED_BUFFER_FRAMES: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// Names of known virtual loopback drivers that can act as a microphone.
const VIRTUAL_HINTS: &[&str] =
    &["blackhole", "loopback", "soundflower", "vb-cable", "vb-audio", "virtual", "soundpad"];

impl DeviceInfo {
    pub fn looks_virtual(&self) -> bool {
        let n = self.name.to_lowercase();
        VIRTUAL_HINTS.iter().any(|h| n.contains(h))
    }
}

fn info(d: &Device) -> Option<DeviceInfo> {
    let id = d.id().ok()?.to_string();
    let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| d.to_string());
    Some(DeviceInfo { id, name })
}

pub fn output_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    host.output_devices().map(|it| it.filter_map(|d| info(&d)).collect()).unwrap_or_default()
}

pub fn input_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    host.input_devices().map(|it| it.filter_map(|d| info(&d)).collect()).unwrap_or_default()
}

pub fn default_output_id() -> Option<String> {
    cpal::default_host().default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string())
}

/// The microphone used for "System default": the system's default input,
/// unless that is a virtual loopback device (BlackHole, Soundpad Virtual Mic…).
/// Mixing one of those into the virtual mic could feed audio back into itself,
/// so then the first real microphone is used instead.
pub fn pick_default_input(default_id: Option<&str>, inputs: &[DeviceInfo]) -> Option<String> {
    let real = |d: &&DeviceInfo| !d.looks_virtual();
    default_id
        .and_then(|id| inputs.iter().find(|d| d.id == id))
        .filter(real)
        .or_else(|| inputs.iter().find(real))
        .map(|d| d.id.clone())
}

pub fn default_input_id() -> Option<String> {
    cpal::default_host().default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string())
}

fn find_device(id: &str) -> Option<Device> {
    let host = cpal::default_host();
    id.parse().ok().and_then(|id| host.device_by_id(&id))
}

/// A running output stream.
pub struct OpenOutput {
    _stream: Stream,
    pub device: DeviceInfo,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: Option<u32>,
}

struct OpenInput {
    _stream: Stream,
    device: DeviceInfo,
    rate: u32,
}

pub struct AudioEngine {
    pub player: Arc<Player>,
    speakers: Option<OpenOutput>,
    mic_out: Option<OpenOutput>,
    input: Option<OpenInput>,
    /// Set by stream error callbacks when a device disappears.
    lost: Arc<AtomicBool>,
    pub last_error: Option<String>,
}

impl AudioEngine {
    pub fn new(player: Arc<Player>) -> Self {
        Self { player, speakers: None, mic_out: None, input: None, lost: Arc::default(), last_error: None }
    }

    pub fn output(&self, k: OutputKind) -> Option<&OpenOutput> {
        match k {
            OutputKind::Speakers => self.speakers.as_ref(),
            OutputKind::VirtualMic => self.mic_out.as_ref(),
        }
    }

    pub fn input_device(&self) -> Option<&DeviceInfo> {
        self.input.as_ref().map(|i| &i.device)
    }

    pub fn input_rate(&self) -> Option<u32> {
        self.input.as_ref().map(|i| i.rate)
    }

    /// True (once) if a stream reported its device went away.
    pub fn take_device_lost(&self) -> bool {
        self.lost.swap(false, Relaxed)
    }

    /// Open (or close with `None`) an output. For speakers, `Some("")` means
    /// "system default".
    pub fn set_output(&mut self, k: OutputKind, device_id: Option<&str>) -> Result<(), String> {
        // Tear down first so the device is released before re-opening.
        self.player.install_output(k, None);
        match k {
            OutputKind::Speakers => self.speakers = None,
            OutputKind::VirtualMic => self.mic_out = None,
        }
        let result = match device_id {
            None => Ok(None),
            Some(id) => {
                let dev = if id.is_empty() {
                    cpal::default_host().default_output_device()
                } else {
                    find_device(id)
                };
                match dev {
                    None => Err(format!("Output device not found: {id}")),
                    Some(dev) => open_output(&dev, self.lost.clone()).map(Some),
                }
            }
        };
        let r = match result {
            Ok(Some((out, handle))) => {
                self.player.install_output(k, Some(handle));
                match k {
                    OutputKind::Speakers => self.speakers = Some(out),
                    OutputKind::VirtualMic => self.mic_out = Some(out),
                }
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if k == OutputKind::VirtualMic {
            // The mic link depends on the virtual output's sample rate.
            let input = self.input.as_ref().map(|i| i.device.id.clone());
            let _ = self.set_input(input.as_deref());
        }
        if let Err(e) = &r {
            self.last_error = Some(e.clone());
        }
        r
    }

    /// Start (or stop with `None`) live microphone passthrough into the virtual mic.
    pub fn set_input(&mut self, device_id: Option<&str>) -> Result<(), String> {
        self.player.set_mic_input(None);
        self.input = None;
        let (Some(id), Some(out)) = (device_id, self.mic_out.as_ref()) else {
            return Ok(());
        };
        let dev = if id.is_empty() {
            pick_default_input(default_input_id().as_deref(), &input_devices()).and_then(|id| find_device(&id))
        } else {
            find_device(id)
        };
        let Some(dev) = dev else {
            let e = if id.is_empty() { "No microphone found".to_string() } else { format!("Input device not found: {id}") };
            self.last_error = Some(e.clone());
            return Err(e);
        };
        match open_input(&dev, out.sample_rate, self.lost.clone()) {
            Ok((stream, mic, device, rate)) => {
                self.player.set_mic_input(Some(mic));
                self.input = Some(OpenInput { _stream: stream, device, rate });
                Ok(())
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }
}

fn err_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn pick_buffer(range: &SupportedBufferSize) -> BufferSize {
    match *range {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(PREFERRED_BUFFER_FRAMES.clamp(min.max(1), max.max(1)))
        }
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

/// Open an output stream on `dev` driven by a fresh mixer.
pub fn open_output(dev: &Device, lost: Arc<AtomicBool>) -> Result<(OpenOutput, MixerHandle), String> {
    let device = info(dev).ok_or("device disappeared")?;
    let def = dev.default_output_config().map_err(err_string)?;
    let rate = def.sample_rate();
    let channels = def.channels();
    let format = def.sample_format();
    let mut last_err = String::new();

    for buffer in [pick_buffer(def.buffer_size()), BufferSize::Default] {
        let config = StreamConfig { channels, sample_rate: rate, buffer_size: buffer };
        let (m, handle) = mixer(rate);
        let status = handle.status.clone();
        let res = match format {
            SampleFormat::F32 => build_output::<f32>(dev, &config, m, status, lost.clone()),
            SampleFormat::I16 => build_output::<i16>(dev, &config, m, status, lost.clone()),
            SampleFormat::I32 => build_output::<i32>(dev, &config, m, status, lost.clone()),
            SampleFormat::U16 => build_output::<u16>(dev, &config, m, status, lost.clone()),
            other => Err(format!("unsupported sample format {other}")),
        };
        match res {
            Ok(stream) => {
                stream.play().map_err(err_string)?;
                let buffer_frames = match buffer {
                    BufferSize::Fixed(n) => Some(n),
                    BufferSize::Default => None,
                };
                let out = OpenOutput { _stream: stream, device, sample_rate: rate, channels, buffer_frames };
                return Ok((out, handle));
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("cannot open \"{}\": {last_err}", device.name))
}

fn build_output<T>(
    dev: &Device,
    config: &StreamConfig,
    mut mixer: Mixer,
    status: Arc<OutputStatus>,
    lost: Arc<AtomicBool>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let mut scratch: Vec<f32> = vec![0.0; 16_384 * channels];
    let is_f32 = T::FORMAT == SampleFormat::F32;
    dev.build_output_stream::<T, _, _>(
        *config,
        move |data: &mut [T], _| {
            let t0 = Instant::now();
            if is_f32 {
                // SAFETY: T is f32 (checked via T::FORMAT); identical layout.
                let data: &mut [f32] =
                    unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr().cast(), data.len()) };
                mixer.render(data, channels);
            } else {
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let s = &mut scratch[..data.len()];
                mixer.render(s, channels);
                for (o, &v) in data.iter_mut().zip(s.iter()) {
                    *o = T::from_sample(v);
                }
            }
            let ns = t0.elapsed().as_nanos() as u64;
            // Budget = real duration of this buffer (devices may not honor the requested size).
            let frames = (data.len() / channels.max(1)) as u64;
            let budget_ns = frames * 1_000_000_000 / rate.max(1) as u64;
            if ns * 2 > budget_ns {
                status.slow_callbacks.fetch_add(1, Relaxed);
            }
            status.callback_frames.store(frames, Relaxed);
            status.last_callback_ns.store(ns, Relaxed);
            status.max_callback_ns.fetch_max(ns, Relaxed);
            status.callbacks.fetch_add(1, Relaxed);
        },
        move |e| {
            use cpal::ErrorKind as K;
            match e.kind() {
                K::Xrun => {}
                K::DeviceNotAvailable | K::DeviceChanged => lost.store(true, Relaxed),
                _ => eprintln!("output stream error: {e}"),
            }
        },
        None,
    )
    .map_err(err_string)
}

fn open_input(
    dev: &Device,
    out_rate: u32,
    lost: Arc<AtomicBool>,
) -> Result<(Stream, Box<MicInput>, DeviceInfo, u32), String> {
    let device = info(dev).ok_or("device disappeared")?;
    let def = dev.default_input_config().map_err(err_string)?;
    let rate = def.sample_rate();
    let channels = def.channels();
    let format = def.sample_format();
    let mut last_err = String::new();
    for buffer in [pick_buffer(def.buffer_size()), BufferSize::Default] {
        let config = StreamConfig { channels, sample_rate: rate, buffer_size: buffer };
        let (prod, mic) = MicInput::new(rate, out_rate);
        let res = match format {
            SampleFormat::F32 => build_input::<f32>(dev, &config, prod, lost.clone()),
            SampleFormat::I16 => build_input::<i16>(dev, &config, prod, lost.clone()),
            SampleFormat::I32 => build_input::<i32>(dev, &config, prod, lost.clone()),
            other => Err(format!("unsupported sample format {other}")),
        };
        match res {
            Ok(stream) => {
                stream.play().map_err(err_string)?;
                return Ok((stream, mic, device, rate));
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("cannot open microphone \"{}\": {last_err}", device.name))
}

fn build_input<T>(
    dev: &Device,
    config: &StreamConfig,
    mut prod: HeapProd<f32>,
    lost: Arc<AtomicBool>,
) -> Result<Stream, String>
where
    T: SizedSample + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let mut stereo: Vec<f32> = vec![0.0; 16_384 * 2];
    dev.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            let frames = data.len() / channels.max(1);
            if stereo.len() < frames * 2 {
                stereo.resize(frames * 2, 0.0);
            }
            let s = &mut stereo[..frames * 2];
            for (o, f) in s.as_chunks_mut::<2>().0.iter_mut().zip(data.chunks_exact(channels.max(1))) {
                let l = <f32 as FromSample<T>>::from_sample_(f[0]);
                let r = if channels > 1 { <f32 as FromSample<T>>::from_sample_(f[1]) } else { l };
                o[0] = l;
                o[1] = r;
            }
            // If the mixer isn't draining (e.g. output stalled) extra audio is dropped.
            prod.push_slice(s);
        },
        move |e| {
            use cpal::ErrorKind as K;
            match e.kind() {
                K::Xrun => {}
                K::DeviceNotAvailable | K::DeviceChanged => lost.store(true, Relaxed),
                _ => eprintln!("input stream error: {e}"),
            }
        },
        None,
    )
    .map_err(err_string)
}

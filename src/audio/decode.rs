//! Decoding audio files into compact in-memory 16-bit buffers.
//!
//! Short sounds (the typical sound-board clip) are fully decoded up front, off
//! the audio thread, so triggering them never touches the disk or a decoder.
//!
//! Long sounds (songs) keep only their first [`HEAD_SECS`] in memory. That head
//! lets them start instantly; while they play, `stream` decodes the rest from
//! disk into a small ring buffer just ahead of the playback position.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{TimeBase, Timestamp};

/// Sounds at most this long are kept fully in memory.
pub const STREAM_MIN_SECS: f64 = 10.0;
/// Seconds of a streamed sound kept in memory so it can start instantly.
pub const HEAD_SECS: f64 = 1.5;

/// Decoded audio kept in memory as 16-bit PCM.
///
/// Mono sources stay mono (`channels == 1`); everything else is stored as
/// stereo interleaved (`[L0, R0, L1, R1, ...]`). Storing `i16` instead of
/// `f32` halves memory use (and mono halves it again); 16 bits is 96 dB of
/// dynamic range, which is transparent for a sound board. The mixer converts
/// to `f32` on the fly.
#[derive(Debug)]
pub struct SoundData {
    /// The whole sound, or for streamed sounds only its first frames (the head).
    pub samples: Box<[i16]>,
    /// 1 (mono) or 2 (stereo interleaved).
    pub channels: u8,
    pub sample_rate: u32,
    /// Length of the whole sound in frames (larger than the head when streamed).
    pub total_frames: usize,
    /// Set when the part after the head is streamed from this file.
    pub stream_path: Option<PathBuf>,
}

/// Scale from `i16` to `f32` in -1..1.
pub const I16_TO_F32: f32 = 1.0 / 32768.0;

#[inline]
pub fn f32_to_i16(v: f32) -> i16 {
    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

impl SoundData {
    /// Build from stereo interleaved `f32` samples (used by tests and the self-test).
    pub fn new(samples: Vec<f32>, sample_rate: u32) -> Self {
        debug_assert!(samples.len().is_multiple_of(2));
        let samples: Box<[i16]> = samples.iter().map(|&v| f32_to_i16(v)).collect();
        let total_frames = samples.len() / 2;
        Self { samples, channels: 2, sample_rate, total_frames, stream_path: None }
    }

    pub fn frames(&self) -> usize {
        self.total_frames
    }

    /// Frames held in memory (all of them unless streamed).
    pub fn head_frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn is_streamed(&self) -> bool {
        self.stream_path.is_some()
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    /// In-memory sample of `frame` for output channel `c` (0 = left, 1 = right).
    #[inline]
    pub fn sample(&self, frame: usize, c: usize) -> f32 {
        let ch = self.channels as usize;
        self.samples[frame * ch + c.min(ch - 1)] as f32 * I16_TO_F32
    }

    /// Bytes of audio held in memory.
    pub fn memory_bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<i16>()
    }
}

pub type SoundRef = Arc<SoundData>;

/// File extensions we offer in the "Add sounds" dialog.
pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["mp3", "wav", "ogg", "oga", "flac", "m4a", "aac", "mp4", "aif", "aiff", "caf", "opus", "webm", "mka"];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// One decoded packet: interleaved samples, channel count, sample rate.
pub type Packet<'a> = (&'a [f32], usize, u32);

/// Packet-by-packet decoder producing 16-bit mono or stereo frames.
pub struct PcmReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    scratch: Vec<f32>,
    pub sample_rate: u32,
    pub num_frames: Option<u64>,
    time_base: Option<TimeBase>,
    /// After a seek: drop decoded frames before this one.
    skip_to: Option<u64>,
}

impl PcmReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| format!("unsupported format: {e}"))?;

        let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
        let track_id = track.id;
        let num_frames = track.num_frames;
        let time_base = track.time_base;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("track has no audio codec parameters")?
            .clone();

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec: {e}"))?;

        Ok(Self {
            format,
            decoder,
            track_id,
            scratch: Vec::new(),
            sample_rate: params.sample_rate.unwrap_or(0),
            num_frames,
            time_base,
            skip_to: None,
        })
    }

    /// Track timestamp → frames (assumes 1/sample_rate when the timebase is unknown).
    fn ts_to_frames(&self, ts: Timestamp) -> u64 {
        let ts = ts.get().max(0) as u128;
        match self.time_base {
            Some(tb) => {
                (ts * tb.numer.get() as u128 * self.sample_rate as u128 / tb.denom.get().max(1) as u128) as u64
            }
            None => ts as u64,
        }
    }

    fn frames_to_ts(&self, frames: u64) -> Timestamp {
        let f = frames as u128;
        let ts = match self.time_base {
            Some(tb) => f * tb.denom.get() as u128 / (tb.numer.get() as u128 * self.sample_rate.max(1) as u128),
            None => f,
        };
        Timestamp::new(ts.min(i64::MAX as u128) as i64)
    }

    /// Jump to `frame`: the next packets start exactly there (within the
    /// precision of the container's timestamps).
    pub fn seek(&mut self, frame: u64) -> Result<(), String> {
        if self.sample_rate == 0 {
            return Err("unknown sample rate".into());
        }
        let ts = self.frames_to_ts(frame);
        self.format
            .seek(SeekMode::Accurate, SeekTo::Timestamp { ts, track_id: self.track_id })
            .map_err(|e| format!("seek failed: {e}"))?;
        self.decoder.reset();
        self.skip_to = Some(frame);
        Ok(())
    }

    /// Decode the next packet. Returns its interleaved `f32` samples, channel
    /// count and sample rate, or `None` at the end of the file.
    pub fn next_packet(&mut self) -> Result<Option<Packet<'_>>, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                // Truncated files: keep whatever we decoded so far.
                Err(SymError::IoError(_)) => return Ok(None),
                Err(e) => return Err(format!("read error: {e}")),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(SymError::DecodeError(_)) => continue, // skip corrupt packet
                Err(e) => return Err(format!("decode error: {e}")),
            };
            let spec = buf.spec();
            let channels = spec.channels().count().max(1);
            if self.sample_rate == 0 {
                self.sample_rate = spec.rate();
            }
            self.scratch.resize(buf.samples_interleaved(), 0.0);
            buf.copy_to_slice_interleaved(&mut self.scratch);
            let mut offset = 0;
            if let Some(target) = self.skip_to {
                // Drop what lies before the seek target.
                let start = self.ts_to_frames(packet.pts);
                let frames = (self.scratch.len() / channels) as u64;
                if start + frames <= target {
                    continue;
                }
                offset = (target.saturating_sub(start) as usize) * channels;
                self.skip_to = None;
            }
            return Ok(Some((&self.scratch[offset..], channels, self.sample_rate)));
        }
    }
}

/// Load a sound: fully when short, otherwise just its head (see module docs).
pub fn decode_file(path: &Path) -> Result<SoundData, String> {
    decode_file_with(path, STREAM_MIN_SECS)
}

/// Like [`decode_file`], streaming sounds longer than `stream_min_secs`
/// (`f64::INFINITY` = always decode fully).
pub fn decode_file_with(path: &Path, stream_min_secs: f64) -> Result<SoundData, String> {
    let mut r = PcmReader::open(path)?;
    let mut out: Vec<i16> = Vec::new();
    // Output layout, fixed by the first decoded packet: 1 = mono, 2 = stereo.
    let mut out_channels = 0usize;
    let mut total_frames = 0usize;
    // Frames to keep in memory once we know the sound is long (0 = keep all so far).
    let mut keep_frames = usize::MAX;
    let mut limit_frames = usize::MAX;
    let num_frames = r.num_frames;

    while let Some((pcm, channels, rate)) = r.next_packet()? {
        let rate = rate.max(1) as f64;
        if out_channels == 0 {
            out_channels = if channels == 1 { 1 } else { 2 };
            if stream_min_secs.is_finite() {
                limit_frames = (stream_min_secs * rate) as usize;
            }
            // Size the buffer once so it never grows by doubling and copying.
            let estimate = num_frames.map(|n| n as usize).unwrap_or(0).min(limit_frames);
            let _ = out.try_reserve_exact(estimate.saturating_mul(out_channels).saturating_add(4096).min(1 << 29));
        }
        let frames = pcm.len() / channels;
        total_frames += frames;
        if keep_frames == usize::MAX {
            append_frames(&mut out, pcm, channels, out_channels);
            if total_frames > limit_frames {
                // Long sound: keep only the head, then just count the rest.
                keep_frames = (HEAD_SECS * rate) as usize;
                out.truncate(keep_frames * out_channels);
                out.shrink_to_fit();
            }
        }
    }

    if r.sample_rate == 0 {
        return Err("unknown sample rate".into());
    }
    if total_frames == 0 || out.is_empty() {
        return Err("file contains no audio".into());
    }
    let streamed = keep_frames != usize::MAX;
    Ok(SoundData {
        samples: out.into_boxed_slice(),
        channels: out_channels as u8,
        sample_rate: r.sample_rate,
        total_frames,
        stream_path: streamed.then(|| path.to_path_buf()),
    })
}

/// Convert interleaved `f32` audio with `channels` channels to `out_channels`
/// (1 or 2) channels of `i16` and append it.
pub fn append_frames(out: &mut Vec<i16>, input: &[f32], channels: usize, out_channels: usize) {
    let frames = input.len() / channels;
    out.reserve(frames * out_channels);
    match (channels, out_channels) {
        (1, 1) | (2, 2) => out.extend(input.iter().map(|&s| f32_to_i16(s))),
        (1, _) => {
            for &s in input {
                let v = f32_to_i16(s);
                out.push(v);
                out.push(v);
            }
        }
        (n, 1) => {
            for frame in input.chunks_exact(n) {
                out.push(f32_to_i16(frame.iter().sum::<f32>() / n as f32));
            }
        }
        (n, _) => {
            // Keep front left/right, fold in center (channel 2 in common layouts).
            for frame in input.chunks_exact(n) {
                let c = if n > 2 { frame[2] * 0.707 } else { 0.0 };
                out.push(f32_to_i16(frame[0] + c));
                out.push(f32_to_i16(frame[1] + c));
            }
        }
    }
}

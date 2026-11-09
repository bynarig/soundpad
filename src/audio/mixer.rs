//! Real-time mixer that runs inside the audio device callback.
//!
//! Rules for everything in `Mixer::render` (the audio thread):
//! - no locks, no allocation, no deallocation, no I/O, no syscalls;
//! - control messages arrive through a lock-free SPSC ring buffer;
//! - finished sounds are handed back through another ring buffer so their
//!   memory is freed on a non-real-time thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::decode::{I16_TO_F32, SoundData, SoundRef};
use super::stream::{self, StreamReader, WINDOW_FRAMES};

/// Maximum number of simultaneously playing sounds per output.
pub const MAX_VOICES: usize = 64;
/// Fade time used for stop/pause/start-of-exclusive to avoid clicks.
pub const FADE_SECS: f32 = 0.006;
/// Capacity of command / garbage queues.
pub const QUEUE_CAPACITY: usize = 1024;
/// Scratch space reserved up front (frames). Larger callbacks grow it once.
const PREALLOC_FRAMES: usize = 8192;

/// f32 stored in an `AtomicU32`.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

/// Parameters the UI can change at any moment (read by the audio thread).
#[derive(Debug)]
pub struct OutputParams {
    /// Volume applied to sounds (0..=2). 0 when the output is disabled.
    pub sound_gain: AtomicF32,
    /// Volume applied to the live microphone passthrough.
    pub mic_gain: AtomicF32,
}

impl Default for OutputParams {
    fn default() -> Self {
        Self { sound_gain: AtomicF32::new(1.0), mic_gain: AtomicF32::new(1.0) }
    }
}

/// Playback state published by the audio thread for the UI.
#[derive(Debug, Default)]
pub struct OutputStatus {
    /// Library id of the most recently started voice that is still playing (0 = none).
    pub sound_id: AtomicU64,
    /// Position of that voice in source frames.
    pub pos_frames: AtomicU64,
    pub total_frames: AtomicU64,
    pub source_rate: AtomicU32,
    pub active_voices: AtomicU32,
    pub paused: AtomicBool,
    /// Peak output level of the last callback (0..1+).
    pub peak: AtomicF32,
    // --- diagnostics ---
    pub callbacks: AtomicU64,
    pub last_callback_ns: AtomicU64,
    pub max_callback_ns: AtomicU64,
    /// Callbacks that used more than half of their real-time budget.
    pub slow_callbacks: AtomicU64,
    /// Frames in the most recent callback (actual device buffer size).
    pub callback_frames: AtomicU64,
    pub mic_underruns: AtomicU64,
    /// Microphone frames mixed in so far, and the mic's level (before gain).
    pub mic_frames: AtomicU64,
    pub mic_peak: AtomicF32,
    pub dropped_commands: AtomicU64,
    /// Incremented when a voice reaches the end of its sound by itself (not
    /// stopped); `ended_sound` is that voice's library id. Drives playlists.
    pub ended_count: AtomicU64,
    pub ended_sound: AtomicU64,
}

pub enum Command {
    /// Start a sound. With `exclusive`, all other voices fade out.
    /// Build it with [`Command::play`], which also opens the stream of long sounds.
    Play { sound_id: u64, sound: SoundRef, exclusive: bool, looping: bool, stream: Option<Box<StreamReader>> },
    /// Jump the newest voice of `sound_id` to `frame` (with a new stream for long sounds).
    Seek { sound_id: u64, frame: usize, stream: Option<Box<StreamReader>> },
    /// Repeat (or stop repeating) every playing voice.
    SetLooping(bool),
    /// Fade out and drop every voice.
    StopAll,
    /// Fade out voices of one library sound.
    Stop { sound_id: u64 },
    SetPaused(bool),
    /// Attach or detach live microphone input.
    SetMicInput(Option<Box<MicInput>>),
}

impl Command {
    /// A `Play` command. For streamed sounds this starts decoding from disk, so
    /// call it on a normal thread (never on the audio thread).
    pub fn play(sound_id: u64, sound: SoundRef, exclusive: bool) -> Self {
        Self::play_with(sound_id, sound, exclusive, false)
    }

    /// Like [`Command::play`], optionally repeating the sound seamlessly.
    pub fn play_with(sound_id: u64, sound: SoundRef, exclusive: bool, looping: bool) -> Self {
        let stream = stream::open(&sound, 0, looping);
        Command::Play { sound_id, sound, exclusive, looping, stream }
    }

    /// A `Seek` command to `frame` of `sound` (non-real-time thread only).
    pub fn seek(sound_id: u64, sound: &SoundData, frame: usize, looping: bool) -> Self {
        let frame = frame.min(sound.total_frames);
        Command::Seek { sound_id, frame, stream: stream::open(sound, frame, looping) }
    }
}

/// Things that must be deallocated off the audio thread.
pub enum Garbage {
    Sound(SoundRef),
    Stream(Box<StreamReader>),
    Mic(Box<MicInput>),
}

/// Control handle used by non-real-time threads.
pub struct MixerHandle {
    commands: HeapProd<Command>,
    garbage: HeapCons<Garbage>,
    pub params: Arc<OutputParams>,
    pub status: Arc<OutputStatus>,
}

impl MixerHandle {
    /// Send a command. Returns `false` if the queue was full (should never happen).
    pub fn send(&mut self, cmd: Command) -> bool {
        self.collect_garbage();
        match self.commands.try_push(cmd) {
            Ok(()) => true,
            Err(_) => {
                self.status.dropped_commands.fetch_add(1, Relaxed);
                false
            }
        }
    }

    /// Free memory released by the audio thread.
    pub fn collect_garbage(&mut self) {
        while let Some(g) = self.garbage.try_pop() {
            drop(g);
        }
    }
}

/// Read position and fade of a voice.
struct PlayState {
    /// Read position in source frames.
    pos: f64,
    /// Source frames advanced per output frame.
    step: f64,
    fade: f32,
    /// Per-frame fade delta (negative while fading out, positive while fading in).
    fade_delta: f32,
}

struct Voice {
    sound: SoundRef,
    /// Decoded audio after the in-memory head, for long sounds.
    stream: Option<Box<StreamReader>>,
    sound_id: u64,
    ps: PlayState,
    /// Fading out because of a stop command (as opposed to reaching the end).
    stopping: bool,
    looping: bool,
    started_seq: u64,
}

const K: f32 = I16_TO_F32;

impl Voice {
    fn start_fade_out(&mut self, delta: f32) {
        if !self.stopping {
            self.stopping = true;
            self.ps.fade_delta = -delta;
        }
    }

    /// Position inside the sound (looping voices count up past the end).
    fn position(&self) -> u64 {
        let total = self.sound.total_frames.max(1) as u64;
        let p = self.ps.pos.max(0.0) as u64;
        if self.looping { p % total } else { p.min(total) }
    }

    /// Mix this voice into the stereo accumulator. Returns `true` when finished.
    #[inline]
    fn render(&mut self, acc: &mut [f32]) -> bool {
        if self.sound.channels == 1 { self.render_ch::<1>(acc) } else { self.render_ch::<2>(acc) }
    }

    /// `CH` is the number of channels stored in the sound (1 = mono, 2 = stereo).
    #[inline(always)]
    fn render_ch<const CH: usize>(&mut self, acc: &mut [f32]) -> bool {
        let head: &[i16] = &self.sound.samples[..];
        let head_frames = head.len() / CH;
        let ch = |c: usize| if CH == 1 { 0 } else { c };

        let Some(st) = self.stream.as_mut() else {
            // Entirely in memory (or a long sound whose stream could not start:
            // play the head only).
            if head_frames == 0 {
                return true;
            }
            let looping = self.looping && !self.sound.is_streamed();
            let n = head_frames as isize;
            let at = |i: isize, c: usize| -> f32 {
                let i = if looping { i.rem_euclid(n) } else { i.clamp(0, n - 1) } as usize;
                head[i * CH + ch(c)] as f32 * K
            };
            let (end, wrap) = if looping { (None, Some(head_frames)) } else { (Some(head_frames), None) };
            return mix::<CH>(&mut self.ps, acc, end, wrap, at);
        };

        // Streamed: the head comes from memory, everything after from the
        // stream window. Positions only grow; when looping, the streamer simply
        // keeps decoding the file again from the start.
        let frames = acc.len() / 2;
        let chunk = ((WINDOW_FRAMES - 8) as f64 / self.ps.step.max(1e-3)).clamp(1.0, 1024.0) as usize;
        let mut f0 = 0;
        while f0 < frames {
            let n = chunk.min(frames - f0);
            let first = (self.ps.pos as usize).saturating_sub(1).max(head_frames);
            let want = (self.ps.pos + n as f64 * self.ps.step) as usize + 3;
            st.ensure(first, want);
            let avail = st.available_end();
            let mut end = None;
            if avail < want {
                if st.finished() {
                    // End of the file (or it became unreadable).
                    end = Some(avail);
                } else {
                    // Disk fell behind: hold position (silence) until data arrives.
                    return false;
                }
            }
            let s: &StreamReader = st;
            let last = end.map_or(isize::MAX, |e| e as isize - 1);
            let at = |i: isize, c: usize| -> f32 {
                let i = i.clamp(0, last.max(0)) as usize;
                if i < head_frames {
                    head[i * CH + ch(c)] as f32 * K
                } else if i < s.available_end() {
                    s.sample(i, c)
                } else {
                    0.0
                }
            };
            if mix::<CH>(&mut self.ps, &mut acc[f0 * 2..(f0 + n) * 2], end, None, at) {
                return true;
            }
            f0 += n;
        }
        false
    }
}

/// Mix frames from `at(frame, channel)` into `acc` with resampling and fades.
/// `end` = frame where the sound ends (`None` = endless), `wrap` = loop length.
/// Returns `true` when the voice is finished.
#[inline(always)]
fn mix<const CH: usize>(
    ps: &mut PlayState,
    acc: &mut [f32],
    end: Option<usize>,
    wrap: Option<usize>,
    at: impl Fn(isize, usize) -> f32,
) -> bool {
    let end = end.map_or(f64::INFINITY, |e| e as f64);
    for f in acc.as_chunks_mut::<2>().0 {
        if ps.pos >= end {
            return true;
        }
        let i = ps.pos as isize;
        let (l, r) = if ps.step == 1.0 {
            let l = at(i, 0);
            (l, if CH == 1 { l } else { at(i, 1) })
        } else {
            // Catmull-Rom cubic interpolation.
            let t = (ps.pos - i as f64) as f32;
            let mut lr = [0.0f32; 2];
            for (c, out) in lr.iter_mut().enumerate().take(CH) {
                let p0 = at(i - 1, c);
                let p1 = at(i, c);
                let p2 = at(i + 1, c);
                let p3 = at(i + 2, c);
                *out = p1
                    + 0.5
                        * t
                        * (p2 - p0
                            + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)));
            }
            if CH == 1 {
                lr[1] = lr[0];
            }
            (lr[0], lr[1])
        };
        f[0] += l * ps.fade;
        f[1] += r * ps.fade;
        ps.pos += ps.step;
        if let Some(w) = wrap
            && ps.pos >= w as f64
        {
            ps.pos -= w as f64;
        }
        if ps.fade_delta != 0.0 {
            ps.fade += ps.fade_delta;
            if ps.fade >= 1.0 {
                ps.fade = 1.0;
                ps.fade_delta = 0.0;
            } else if ps.fade <= 0.0 {
                return true;
            }
        }
    }
    ps.pos >= end
}

/// Live microphone input feeding one output (the virtual mic).
///
/// The capture callback pushes stereo interleaved frames at `in_rate`; the
/// mixer pulls them at the output rate with linear interpolation and keeps the
/// buffered latency between `target_fill` and `max_fill` (clock drift between
/// two independent devices would otherwise grow latency or cause gaps).
pub struct MicInput {
    cons: HeapCons<f32>,
    ratio: f64,
    frac: f64,
    prev: [f32; 2],
    next: [f32; 2],
    primed: bool,
    target_fill: usize,
    max_fill: usize,
}

impl MicInput {
    /// Create a mic link. Returns the producer the capture callback writes to.
    pub fn new(in_rate: u32, out_rate: u32) -> (HeapProd<f32>, Box<MicInput>) {
        // ~0.5 s of buffer space; aim for ~15 ms latency, cap at ~60 ms.
        let cap = (in_rate as usize / 2).max(4096) * 2;
        let (prod, cons) = HeapRb::<f32>::new(cap).split();
        let frames_per_ms = in_rate as usize / 1000;
        let mic = Box::new(MicInput {
            cons,
            ratio: in_rate as f64 / out_rate as f64,
            frac: 1.0,
            prev: [0.0; 2],
            next: [0.0; 2],
            primed: false,
            target_fill: frames_per_ms * 15 * 2,
            max_fill: frames_per_ms * 60 * 2,
        });
        (prod, mic)
    }

    #[inline]
    fn pop_frame(&mut self) -> Option<[f32; 2]> {
        if self.cons.occupied_len() < 2 {
            return None;
        }
        let mut f = [0.0f32; 2];
        self.cons.pop_slice(&mut f);
        Some(f)
    }

    /// Mix into `acc` with a gain ramp. Returns (underrun, frames mixed, input peak).
    fn render(&mut self, acc: &mut [f32], g0: f32, g1: f32) -> (bool, usize, f32) {
        let fill = self.cons.occupied_len();
        let mut peak = 0.0f32;
        if !self.primed {
            if fill < self.target_fill {
                return (false, 0, 0.0);
            }
            self.primed = true;
        }
        if fill > self.max_fill {
            // Too much latency accumulated: jump forward to the target.
            let skip = (fill - self.target_fill) & !1;
            self.cons.skip(skip);
        }
        let frames = acc.len() / 2;
        let dg = (g1 - g0) / frames.max(1) as f32;
        let mut g = g0;
        for f in 0..frames {
            while self.frac >= 1.0 {
                match self.pop_frame() {
                    Some(n) => {
                        self.prev = self.next;
                        self.next = n;
                        self.frac -= 1.0;
                    }
                    None => {
                        // Underrun: wait until the buffer refills before resuming.
                        self.primed = false;
                        return (true, f, peak);
                    }
                }
            }
            let t = self.frac as f32;
            let l = self.prev[0] + (self.next[0] - self.prev[0]) * t;
            let r = self.prev[1] + (self.next[1] - self.prev[1]) * t;
            peak = peak.max(l.abs()).max(r.abs());
            acc[f * 2] += l * g;
            acc[f * 2 + 1] += r * g;
            self.frac += self.ratio;
            g += dg;
        }
        (false, frames, peak)
    }
}

pub struct Mixer {
    out_rate: u32,
    voices: Vec<Voice>,
    commands: HeapCons<Command>,
    garbage: HeapProd<Garbage>,
    params: Arc<OutputParams>,
    status: Arc<OutputStatus>,
    acc: Vec<f32>,
    mic: Option<Box<MicInput>>,
    paused: bool,
    /// 0..1 ramp used to fade in/out on pause.
    pause_gain: f32,
    cur_sound_gain: f32,
    cur_mic_gain: f32,
    seq: u64,
    fade_delta: f32,
}

/// Create a mixer for an output running at `out_rate`, plus its control handle.
pub fn mixer(out_rate: u32) -> (Mixer, MixerHandle) {
    let (cmd_prod, cmd_cons) = HeapRb::<Command>::new(QUEUE_CAPACITY).split();
    let (gar_prod, gar_cons) = HeapRb::<Garbage>::new(QUEUE_CAPACITY).split();
    let params = Arc::new(OutputParams::default());
    let status = Arc::new(OutputStatus::default());
    let fade_frames = (FADE_SECS * out_rate as f32).max(1.0);
    let m = Mixer {
        out_rate,
        voices: Vec::with_capacity(MAX_VOICES),
        commands: cmd_cons,
        garbage: gar_prod,
        params: params.clone(),
        status: status.clone(),
        acc: vec![0.0; PREALLOC_FRAMES * 2],
        mic: None,
        paused: false,
        pause_gain: 1.0,
        cur_sound_gain: params.sound_gain.get(),
        cur_mic_gain: params.mic_gain.get(),
        seq: 0,
        fade_delta: 1.0 / fade_frames,
    };
    let h = MixerHandle { commands: cmd_prod, garbage: gar_cons, params, status };
    (m, h)
}

impl Mixer {
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    fn trash(&mut self, g: Garbage) {
        // The queue is sized far beyond MAX_VOICES; if it is somehow full we
        // have no choice but to free here.
        let _ = self.garbage.try_push(g);
    }

    fn trash_voice(&mut self, v: Voice) {
        self.trash(Garbage::Sound(v.sound));
        if let Some(s) = v.stream {
            self.trash(Garbage::Stream(s));
        }
    }

    fn handle_commands(&mut self) {
        while let Some(cmd) = self.commands.try_pop() {
            match cmd {
                Command::Play { sound_id, sound, exclusive, looping, stream } => {
                    if exclusive {
                        let d = self.fade_delta;
                        self.voices.iter_mut().for_each(|v| v.start_fade_out(d));
                    }
                    if self.voices.len() == MAX_VOICES {
                        // Steal the oldest voice.
                        let old = self.voices.remove(0);
                        self.trash_voice(old);
                    }
                    self.seq += 1;
                    let step = sound.sample_rate as f64 / self.out_rate as f64;
                    self.voices.push(Voice {
                        sound,
                        stream,
                        sound_id,
                        ps: PlayState { pos: 0.0, step, fade: 1.0, fade_delta: 0.0 },
                        stopping: false,
                        looping,
                        started_seq: self.seq,
                    });
                    // Starting a sound un-pauses, like Soundpad.
                    self.paused = false;
                }
                Command::StopAll => {
                    let d = self.fade_delta;
                    self.voices.iter_mut().for_each(|v| v.start_fade_out(d));
                    self.paused = false;
                }
                Command::Stop { sound_id } => {
                    let d = self.fade_delta;
                    self.voices
                        .iter_mut()
                        .filter(|v| v.sound_id == sound_id)
                        .for_each(|v| v.start_fade_out(d));
                }
                Command::SetPaused(p) => self.paused = p,
                Command::Seek { sound_id, frame, stream } => {
                    let fade_in = self.fade_delta;
                    let target = self
                        .voices
                        .iter_mut()
                        .filter(|v| v.sound_id == sound_id && !v.stopping)
                        .max_by_key(|v| v.started_seq);
                    let unused = match target {
                        Some(v) => {
                            v.ps.pos = frame as f64;
                            // Short fade-in so the jump doesn't click.
                            v.ps.fade = 0.0;
                            v.ps.fade_delta = fade_in;
                            std::mem::replace(&mut v.stream, stream)
                        }
                        None => stream,
                    };
                    if let Some(old) = unused {
                        self.trash(Garbage::Stream(old));
                    }
                }
                Command::SetLooping(on) => {
                    for v in &mut self.voices {
                        v.looping = on;
                        if let Some(s) = &v.stream {
                            s.set_looping(on);
                        }
                    }
                }
                Command::SetMicInput(m) => {
                    if let Some(old) = std::mem::replace(&mut self.mic, m) {
                        self.trash(Garbage::Mic(old));
                    }
                }
            }
        }
    }

    /// Fill `out` (interleaved, `channels` channels) with the next block of audio.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        self.handle_commands();

        if self.acc.len() < frames * 2 {
            // Only happens if the device suddenly uses a huge buffer.
            self.acc.resize(frames * 2, 0.0);
        }
        let mut acc = std::mem::take(&mut self.acc);
        let a = &mut acc[..frames * 2];
        a.fill(0.0);

        // --- sounds ---
        let target_pause = if self.paused { 0.0 } else { 1.0 };
        let mut i = 0;
        let mut newest: Option<usize> = None;
        let sounds_audible = self.pause_gain > 0.0 || target_pause > 0.0;
        if sounds_audible {
            while i < self.voices.len() {
                if self.voices[i].render(a) {
                    let v = self.voices.swap_remove(i);
                    if !v.stopping {
                        self.status.ended_sound.store(v.sound_id, Relaxed);
                        self.status.ended_count.fetch_add(1, Relaxed);
                    }
                    self.trash_voice(v);
                } else {
                    i += 1;
                }
            }
        }
        for (idx, v) in self.voices.iter().enumerate() {
            if !v.stopping
                && newest.is_none_or(|n| self.voices[n].started_seq < v.started_seq)
            {
                newest = Some(idx);
            }
        }

        // Pause ramp + volume ramp applied together.
        let sg1 = self.params.sound_gain.get();
        let sg0 = self.cur_sound_gain;
        let fd = self.fade_delta;
        if sounds_audible {
            let dg = (sg1 - sg0) / frames.max(1) as f32;
            let mut g = sg0;
            let mut pg = self.pause_gain;
            for f in a.as_chunks_mut::<2>().0 {
                let k = g * pg;
                f[0] *= k;
                f[1] *= k;
                g += dg;
                if pg < target_pause {
                    pg = (pg + fd).min(1.0);
                } else if pg > target_pause {
                    pg = (pg - fd).max(0.0);
                }
            }
            self.pause_gain = pg;
        }
        self.cur_sound_gain = sg1;

        // --- live mic ---
        let mg1 = self.params.mic_gain.get();
        if let Some(mic) = self.mic.as_mut() {
            let (underrun, n, mic_peak) = mic.render(a, self.cur_mic_gain, mg1);
            if underrun {
                self.status.mic_underruns.fetch_add(1, Relaxed);
            }
            self.status.mic_frames.fetch_add(n as u64, Relaxed);
            self.status.mic_peak.set(mic_peak);
        }
        self.cur_mic_gain = mg1;

        // --- write out with soft limiting ---
        let mut peak = 0.0f32;
        match channels {
            1 => {
                for (o, f) in out.iter_mut().zip(a.chunks_exact(2)) {
                    let s = soft_clip((f[0] + f[1]) * 0.5);
                    peak = peak.max(s.abs());
                    *o = s;
                }
            }
            _ => {
                for (o, f) in out.chunks_exact_mut(channels).zip(a.chunks_exact(2)) {
                    let l = soft_clip(f[0]);
                    let r = soft_clip(f[1]);
                    peak = peak.max(l.abs()).max(r.abs());
                    o[0] = l;
                    o[1] = r;
                    o[2..].fill(0.0);
                }
            }
        }
        self.acc = acc;

        // --- publish status ---
        let st = &self.status;
        st.peak.set(peak);
        st.paused.store(self.paused, Relaxed);
        st.active_voices.store(self.voices.len() as u32, Relaxed);
        match newest.map(|n| &self.voices[n]) {
            Some(v) => {
                st.sound_id.store(v.sound_id, Relaxed);
                st.pos_frames.store(v.position(), Relaxed);
                st.total_frames.store(v.sound.frames() as u64, Relaxed);
                st.source_rate.store(v.sound.sample_rate, Relaxed);
            }
            None => st.sound_id.store(0, Relaxed),
        }
    }
}

/// Transparent below 0.9, smoothly saturates towards ±1 above it.
#[inline]
fn soft_clip(x: f32) -> f32 {
    const T: f32 = 0.9;
    let a = x.abs();
    if a <= T {
        x
    } else {
        let over = (a - T) / (1.0 - T);
        let y = T + (1.0 - T) * (over / (1.0 + over));
        y.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode::SoundData;

    fn sound(frames: usize, rate: u32, value: f32) -> SoundRef {
        Arc::new(SoundData::new(vec![value; frames * 2], rate))
    }

    fn run(m: &mut Mixer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        m.render(&mut out, 2);
        out
    }

    /// Sound whose sample at frame `i` is `i` (as i16), so positions are visible.
    fn ramp(frames: usize) -> SoundRef {
        let samples: Box<[i16]> = (0..frames).flat_map(|i| [i as i16, i as i16]).collect();
        Arc::new(SoundData { samples, channels: 2, sample_rate: 48_000, total_frames: frames, stream_path: None })
    }

    fn frame_of(v: f32) -> i32 {
        (v * 32768.0).round() as i32
    }

    #[test]
    fn seek_jumps_with_a_short_fade_in() {
        let (mut m, mut h) = mixer(48_000);
        let s = ramp(20_000);
        h.send(Command::play(1, s.clone(), true));
        run(&mut m, 100);
        h.send(Command::seek(1, &s, 10_000, false));
        let out = run(&mut m, 1000);
        // Fades in from silence (no click), then plays from frame 10000 on.
        assert_eq!(out[0], 0.0);
        let f = 500;
        assert_eq!(frame_of(out[f * 2]), 10_000 + f as i32);
        assert_eq!(h.status.pos_frames.load(Relaxed), 11_000);
        // Seeking past the end finishes the sound.
        h.send(Command::seek(1, &s, 1_000_000, false));
        run(&mut m, 16);
        assert_eq!(h.status.active_voices.load(Relaxed), 0);
    }

    #[test]
    fn looping_repeats_seamlessly_until_turned_off() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play_with(1, ramp(1000), true, true));
        let out = run(&mut m, 3500);
        for f in 0..3500 {
            assert_eq!(frame_of(out[f * 2]), (f % 1000) as i32, "frame {f}");
        }
        assert_eq!(h.status.pos_frames.load(Relaxed), 500);
        assert_eq!(h.status.ended_count.load(Relaxed), 0);
        // Turning loop off lets the current pass finish.
        h.send(Command::SetLooping(false));
        let out = run(&mut m, 1000);
        assert_eq!(frame_of(out[499 * 2]), 999);
        assert_eq!(out[500 * 2], 0.0);
        assert_eq!(h.status.active_voices.load(Relaxed), 0);
        assert_eq!(h.status.ended_count.load(Relaxed), 1);
        assert_eq!(h.status.ended_sound.load(Relaxed), 1);
    }

    #[test]
    fn stopping_is_not_reported_as_reaching_the_end() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, ramp(10_000), true));
        run(&mut m, 100);
        h.send(Command::StopAll);
        run(&mut m, 1000);
        assert_eq!(h.status.active_voices.load(Relaxed), 0);
        assert_eq!(h.status.ended_count.load(Relaxed), 0);
    }

    #[test]
    fn plays_sound_at_same_rate() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(1000, 48_000, 0.5), true));
        let out = run(&mut m, 512);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert_eq!(h.status.sound_id.load(Relaxed), 1);
        assert_eq!(h.status.pos_frames.load(Relaxed), 512);
        // remainder then silence
        let out = run(&mut m, 512);
        assert!(out[..488 * 2].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert!(out[488 * 2..].iter().all(|&s| s == 0.0));
        assert_eq!(h.status.sound_id.load(Relaxed), 0);
        assert_eq!(h.status.active_voices.load(Relaxed), 0);
        // finished sound came back for deallocation on this thread
        h.collect_garbage();
    }

    #[test]
    fn resamples_to_output_rate() {
        // 1 s at 44.1k played on a 48k device must last ~48000 output frames.
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(7, sound(44_100, 44_100, 0.25), true));
        let mut produced = 0;
        for _ in 0..200 {
            let out = run(&mut m, 480);
            produced += out.chunks(2).filter(|f| f[0] != 0.0).count();
            if h.status.active_voices.load(Relaxed) == 0 {
                break;
            }
        }
        assert!((produced as i64 - 48_000).abs() <= 2, "produced {produced}");
    }

    #[test]
    fn exclusive_play_fades_out_previous() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(48_000, 48_000, 0.3), true));
        run(&mut m, 256);
        h.send(Command::play(2, sound(48_000, 48_000, 0.3), true));
        let out = run(&mut m, 512);
        // first sample: both voices (old at fade ~1) — no hard jump to 0.
        assert!(out[0] > 0.5);
        // after the fade only the new voice remains
        // Sounds are stored as 16-bit, so allow one quantization step.
        assert!((out[1022] - 0.3).abs() < 1e-4);
        assert_eq!(h.status.active_voices.load(Relaxed), 1);
        assert_eq!(h.status.sound_id.load(Relaxed), 2);
    }

    #[test]
    fn mono_sounds_play_on_both_channels() {
        // Mono files are stored with one channel; check both fast and resampled paths.
        for rate in [48_000, 44_100] {
            let mono = Arc::new(SoundData {
                samples: vec![9830i16; 4800].into(),
                channels: 1,
                sample_rate: rate,
                total_frames: 4800,
                stream_path: None,
            });
            let (mut m, mut h) = mixer(48_000);
            h.send(Command::play(1, mono, false));
            let out = run(&mut m, 256);
            for f in out[200..400].chunks(2) {
                assert!((f[0] - 0.3).abs() < 1e-4 && f[0] == f[1], "rate {rate}: {f:?}");
            }
        }
    }

    #[test]
    fn overlap_mode_mixes_voices() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(4800, 48_000, 0.2), false));
        h.send(Command::play(2, sound(4800, 48_000, 0.2), false));
        let out = run(&mut m, 64);
        assert!((out[10] - 0.4).abs() < 1e-4);
        assert_eq!(h.status.active_voices.load(Relaxed), 2);
    }

    #[test]
    fn stop_all_fades_without_click() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(48_000, 48_000, 0.5), true));
        run(&mut m, 128);
        h.send(Command::StopAll);
        let out = run(&mut m, 1024);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        for w in left.windows(2) {
            assert!((w[0] - w[1]).abs() < 0.01, "click: {} -> {}", w[0], w[1]);
        }
        assert_eq!(*left.last().unwrap(), 0.0);
        assert_eq!(h.status.active_voices.load(Relaxed), 0);
    }

    #[test]
    fn pause_and_resume_keep_position() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(48_000, 48_000, 0.5), true));
        run(&mut m, 1000);
        h.send(Command::SetPaused(true));
        run(&mut m, 1000); // fade out (advances a few hundred frames)
        let pos = h.status.pos_frames.load(Relaxed);
        let out = run(&mut m, 1000);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(h.status.pos_frames.load(Relaxed), pos, "position must not move while paused");
        assert!(h.status.paused.load(Relaxed));
        h.send(Command::SetPaused(false));
        let out = run(&mut m, 1000);
        assert!((out[1998] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn volume_changes_are_smoothed() {
        let (mut m, h) = mixer(48_000);
        let mut h = h;
        h.send(Command::play(1, sound(48_000, 48_000, 0.5), true));
        run(&mut m, 256);
        h.params.sound_gain.set(0.0);
        let out = run(&mut m, 256);
        assert!(out[0] > 0.49 && out[510] < 0.01);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        for w in left.windows(2) {
            assert!((w[0] - w[1]).abs() < 0.005);
        }
    }

    #[test]
    fn mono_and_multichannel_devices() {
        let (mut m, mut h) = mixer(48_000);
        h.send(Command::play(1, sound(4800, 48_000, 0.5), true));
        let mut mono = vec![0.0; 64];
        m.render(&mut mono, 1);
        assert!(mono.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        let mut six = vec![9.0; 64 * 6];
        m.render(&mut six, 6);
        for f in six.chunks(6) {
            assert!((f[0] - 0.5).abs() < 1e-6 && (f[1] - 0.5).abs() < 1e-6);
            assert!(f[2..].iter().all(|&s| s == 0.0));
        }
    }

    #[test]
    fn output_never_exceeds_full_scale() {
        let (mut m, mut h) = mixer(48_000);
        for id in 0..20 {
            h.send(Command::play(id, sound(4800, 48_000, 0.9), false));
        }
        let out = run(&mut m, 256);
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn voice_limit_steals_oldest() {
        let (mut m, mut h) = mixer(48_000);
        for id in 1..=(MAX_VOICES as u64 + 10) {
            h.send(Command::play(id, sound(48_000, 48_000, 0.01), false));
        }
        run(&mut m, 64);
        assert_eq!(h.status.active_voices.load(Relaxed), MAX_VOICES as u32);
        assert_eq!(h.status.sound_id.load(Relaxed), MAX_VOICES as u64 + 10);
    }

    #[test]
    fn mic_passthrough_flows_and_bounds_latency() {
        let (mut m, mut h) = mixer(48_000);
        let (mut prod, mic) = MicInput::new(48_000, 48_000);
        h.send(Command::SetMicInput(Some(mic)));
        // Not primed yet: silence until ~15 ms is buffered.
        prod.push_slice(&[0.25; 200]);
        assert!(run(&mut m, 64).iter().all(|&s| s == 0.0));
        prod.push_slice(&vec![0.25; 48 * 30 * 2]);
        let out = run(&mut m, 256);
        assert!(out[100..].iter().all(|&s| (s - 0.25).abs() < 1e-5));
        // Flood the buffer (simulates clock drift); latency must be clamped.
        prod.push_slice(&vec![0.25; 48 * 200 * 2]);
        run(&mut m, 256);
        let fill = m.mic.as_ref().unwrap().cons.occupied_len();
        assert!(fill <= 48 * 60 * 2, "mic latency not bounded: {fill} samples");
    }

    #[test]
    fn mic_resamples_between_rates() {
        let (mut m, mut h) = mixer(48_000);
        let (mut prod, mic) = MicInput::new(44_100, 48_000);
        h.send(Command::SetMicInput(Some(mic)));
        prod.push_slice(&vec![0.1; 44_100 * 2 / 10]); // 100 ms
        let before = prod.occupied_len();
        run(&mut m, 4800); // 100 ms of output
        let consumed = before - prod.occupied_len();
        // ~4410 input frames consumed for 4800 output frames
        assert!((consumed as i64 / 2 - 4410).abs() <= 3, "consumed {}", consumed / 2);
    }

    #[test]
    fn soft_clip_is_monotonic_and_bounded() {
        let mut prev = -10.0f32;
        for i in -400..=400 {
            let y = soft_clip(i as f32 / 100.0);
            assert!(y >= prev && y.abs() < 1.0 + 1e-6);
            prev = y;
        }
        assert_eq!(soft_clip(0.5), 0.5);
    }
}

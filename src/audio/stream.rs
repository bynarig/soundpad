//! Streaming playback of long sounds.
//!
//! A streamed [`SoundData`] holds only its first frames (the head) in memory.
//! When it is played, [`open`] creates a [`StreamReader`] for the mixer plus a
//! job for the background streamer thread. That thread decodes the file from
//! disk and keeps a small lock-free ring buffer about [`RING_SECS`] ahead of
//! playback. The mixer plays the head from memory while the ring fills, so
//! starting a song is just as instant as starting a short clip.
//!
//! Real-time rules: the mixer only pops from the ring and copies within a
//! buffer allocated up front. The reader is freed off the audio thread through
//! the mixer's garbage queue; dropping it tells the streamer to stop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::decode::{I16_TO_F32, PcmReader, SoundData, append_frames};

/// How far ahead of playback the streamer decodes.
pub const RING_SECS: f64 = 2.0;
/// Frames of decoded audio the mixer can look at in one go.
pub const WINDOW_FRAMES: usize = 4096;

#[derive(Default)]
struct Shared {
    /// Cleared when the reader is dropped (playback ended): the streamer stops.
    alive: AtomicBool,
    /// Set by the streamer at end of file (or on error): no more data will come.
    done: AtomicBool,
    /// At end of file, continue from the start (seamless repeat).
    looping: AtomicBool,
}

/// Consumer side, owned by a mixer voice.
pub struct StreamReader {
    cons: HeapCons<i16>,
    shared: Arc<Shared>,
    channels: usize,
    /// Decoded frames `[win_start, win_start + win_len)`, interleaved.
    win: Box<[i16]>,
    win_start: usize,
    win_len: usize,
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.shared.alive.store(false, Relaxed);
    }
}

impl StreamReader {
    /// Make frames `[from, to)` available in the window as far as the ring
    /// allows (real-time safe). `to - from` must fit in [`WINDOW_FRAMES`].
    pub fn ensure(&mut self, from: usize, to: usize) {
        let ch = self.channels;
        // Forget frames before `from`.
        if from > self.win_start {
            let drop = (from - self.win_start).min(self.win_len);
            self.win.copy_within(drop * ch..self.win_len * ch, 0);
            self.win_len -= drop;
            self.win_start += drop;
            if self.win_len == 0 && from > self.win_start {
                let avail = self.cons.occupied_len() / ch;
                let skip = (from - self.win_start).min(avail);
                self.cons.skip(skip * ch);
                self.win_start += skip;
            }
        }
        // Pull new frames from the ring.
        let end = self.win_start + self.win_len;
        if to > end {
            let want = (to - end).min(WINDOW_FRAMES - self.win_len).min(self.cons.occupied_len() / ch);
            let n = self.cons.pop_slice(&mut self.win[self.win_len * ch..(self.win_len + want) * ch]);
            self.win_len += n / ch;
        }
    }

    /// First frame not yet available.
    #[inline]
    pub fn available_end(&self) -> usize {
        self.win_start + self.win_len
    }

    /// The streamer has finished: nothing beyond `available_end` + ring will arrive.
    pub fn finished(&self) -> bool {
        self.shared.done.load(Relaxed) && self.cons.occupied_len() < self.channels
    }

    /// Sample of `frame` for output channel `c`. `frame` must be below
    /// [`available_end`](Self::available_end); frames before the window (e.g.
    /// the interpolation tap just before a seek target) read its first frame.
    #[inline]
    pub fn sample(&self, frame: usize, c: usize) -> f32 {
        let ch = self.channels;
        let k = frame.saturating_sub(self.win_start).min(self.win_len.saturating_sub(1));
        self.win[k * ch + c.min(ch - 1)] as f32 * I16_TO_F32
    }

    /// Repeat the file when it ends (real-time safe).
    pub fn set_looping(&self, on: bool) {
        self.shared.looping.store(on, Relaxed);
    }

    /// Frames buffered ahead (ring + window). For diagnostics and tests.
    pub fn buffered_frames(&self) -> usize {
        self.win_len + self.cons.occupied_len() / self.channels
    }
}

struct Job {
    path: PathBuf,
    prod: HeapProd<i16>,
    shared: Arc<Shared>,
    channels: usize,
    /// First frame to deliver (the head, or a seek target).
    start: usize,
}

/// Seeks closer than this are done by decoding and discarding (bit-exact);
/// farther ones jump in the file.
const DECODE_SKIP_SECS: f64 = 15.0;

struct Active {
    job: Job,
    reader: Option<PcmReader>,
    /// Decoded frames still to discard before delivering.
    skip: usize,
    /// Converted samples not yet pushed into the ring.
    pending: Vec<i16>,
    pending_pos: usize,
}

impl Active {
    fn finish(&mut self) {
        self.job.shared.done.store(true, Relaxed);
        self.reader = None;
    }

    /// Fill the ring as much as possible. Returns true when the job is over.
    fn pump(&mut self) -> bool {
        let shared = &self.job.shared;
        if !shared.alive.load(Relaxed) {
            return true;
        }
        if self.reader.is_none() {
            match PcmReader::open(&self.job.path) {
                Ok(mut r) => {
                    let start = std::mem::take(&mut self.job.start);
                    let near = (DECODE_SKIP_SECS * r.sample_rate.max(1) as f64) as usize;
                    self.skip = start;
                    if start > near && r.seek(start as u64).is_ok() {
                        self.skip = 0;
                    } else if start > near {
                        // Not seekable: start over and decode up to the target.
                        match PcmReader::open(&self.job.path) {
                            Ok(fresh) => r = fresh,
                            Err(e) => {
                                eprintln!("stream {:?}: {e}", self.job.path);
                                self.finish();
                                return true;
                            }
                        }
                    }
                    self.reader = Some(r);
                }
                Err(e) => {
                    eprintln!("stream {:?}: {e}", self.job.path);
                    self.finish();
                    return true;
                }
            }
        }
        loop {
            if !self.flush() {
                return false; // ring full
            }
            let Some(r) = self.reader.as_mut() else { return true };
            match r.next_packet() {
                Ok(Some((pcm, channels, _))) => {
                    self.pending.clear();
                    self.pending_pos = 0;
                    append_frames(&mut self.pending, pcm, channels, self.job.channels);
                    if self.skip > 0 {
                        let frames = self.pending.len() / self.job.channels;
                        let n = self.skip.min(frames);
                        self.skip -= n;
                        self.pending_pos = n * self.job.channels;
                    }
                }
                Ok(None) if self.job.shared.looping.load(Relaxed) => {
                    // Seamless repeat: keep going from the first frame.
                    self.reader = None;
                    match PcmReader::open(&self.job.path) {
                        Ok(r) => self.reader = Some(r),
                        Err(_) => {
                            self.finish();
                            return true;
                        }
                    }
                }
                Ok(None) => {
                    self.finish();
                    return true;
                }
                Err(e) => {
                    eprintln!("stream {:?}: {e}", self.job.path);
                    self.finish();
                    return true;
                }
            }
        }
    }

    /// Push pending samples. Returns true when everything pending was pushed.
    fn flush(&mut self) -> bool {
        let ch = self.job.channels;
        let rest = &self.pending[self.pending_pos..];
        // Only push whole frames.
        let room = self.job.prod.vacant_len() / ch * ch;
        let n = self.job.prod.push_slice(&rest[..rest.len().min(room)]);
        self.pending_pos += n;
        self.pending_pos >= self.pending.len()
    }
}

fn streamer() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Job>();
        std::thread::Builder::new()
            .name("streamer".into())
            .spawn(move || run(rx))
            .expect("spawn streamer thread");
        tx
    })
}

fn run(rx: Receiver<Job>) {
    let mut active: Vec<Active> = Vec::new();
    loop {
        // Block while idle; otherwise poll often enough to stay ahead.
        let next = if active.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(Duration::from_millis(10))
        };
        match next {
            Ok(job) => active.push(Active { job, reader: None, skip: 0, pending: Vec::new(), pending_pos: 0 }),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(job) = rx.try_recv() {
            active.push(Active { job, reader: None, skip: 0, pending: Vec::new(), pending_pos: 0 });
        }
        active.retain_mut(|a| !a.pump());
    }
}

/// Start streaming `sound` from frame `start` (at least from the end of its
/// in-memory head). Call from a normal (non-real-time) thread; returns `None`
/// for sounds held fully in memory.
pub fn open(sound: &SoundData, start: usize, looping: bool) -> Option<Box<StreamReader>> {
    let path = sound.stream_path.clone()?;
    let ch = sound.channels.max(1) as usize;
    let ring_frames = ((RING_SECS * sound.sample_rate as f64) as usize).max(WINDOW_FRAMES * 2);
    let (prod, cons) = HeapRb::<i16>::new(ring_frames * ch).split();
    let shared = Arc::new(Shared {
        alive: AtomicBool::new(true),
        done: AtomicBool::new(false),
        looping: AtomicBool::new(looping),
    });
    let head = start.max(sound.head_frames());
    let reader = Box::new(StreamReader {
        cons,
        shared: shared.clone(),
        channels: ch,
        win: vec![0; WINDOW_FRAMES * ch].into_boxed_slice(),
        win_start: head,
        win_len: 0,
    });
    let job = Job { path, prod, shared, channels: ch, start: head };
    if streamer().send(job).is_err() {
        return None;
    }
    Some(reader)
}

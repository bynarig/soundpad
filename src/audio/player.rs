//! Thread-safe front end used by the UI and by global hotkeys.
//!
//! `Player` never touches cpal streams; it only pushes commands into the
//! mixers' lock-free queues, so a hotkey can trigger a sound from any thread
//! without waiting for the UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use super::decode::SoundRef;
use super::mixer::{AtomicF32, Command, MicInput, MixerHandle, OutputStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputKind {
    /// What you hear (headphones / speakers).
    Speakers,
    /// The virtual cable that other apps use as a microphone.
    VirtualMic,
}

/// What happens when a sound reaches its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Play the sound once and stop.
    #[default]
    Once,
    /// Repeat the sound seamlessly.
    Loop,
    /// Continue with the next sound of the current list (wrapping around).
    Playlist,
}

impl PlaybackMode {
    fn to_u8(self) -> u8 {
        self as u8
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Loop,
            2 => Self::Playlist,
            _ => Self::Once,
        }
    }
}

/// Snapshot of what is currently playing.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaybackInfo {
    pub sound_id: u64,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub paused: bool,
    pub active_voices: u32,
    pub peak: f32,
}

#[derive(Default)]
struct Outputs {
    speakers: Option<MixerHandle>,
    mic: Option<MixerHandle>,
}

impl Outputs {
    fn get(&mut self, k: OutputKind) -> &mut Option<MixerHandle> {
        match k {
            OutputKind::Speakers => &mut self.speakers,
            OutputKind::VirtualMic => &mut self.mic,
        }
    }
    fn each(&mut self) -> impl Iterator<Item = &mut MixerHandle> {
        self.speakers.iter_mut().chain(self.mic.iter_mut())
    }
}

pub struct Player {
    outputs: Mutex<Outputs>,
    bank: RwLock<HashMap<u64, SoundRef>>,
    pub overlap: AtomicBool,
    paused: AtomicBool,
    speakers_enabled: AtomicBool,
    mic_enabled: AtomicBool,
    speaker_volume: AtomicF32,
    mic_volume: AtomicF32,
    passthrough_volume: AtomicF32,
    on_change: RwLock<Option<Box<dyn Fn() + Send + Sync>>>,
    mode: AtomicU8,
    /// Order used by next/previous and playlist mode (library ids).
    queue: RwLock<Vec<u64>>,
    /// The sound most recently started (0 = none).
    current: AtomicU64,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            outputs: Mutex::default(),
            bank: RwLock::default(),
            overlap: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            speakers_enabled: AtomicBool::new(true),
            mic_enabled: AtomicBool::new(true),
            speaker_volume: AtomicF32::new(0.8),
            mic_volume: AtomicF32::new(0.8),
            passthrough_volume: AtomicF32::new(1.0),
            on_change: RwLock::new(None),
            mode: AtomicU8::new(0),
            queue: RwLock::default(),
            current: AtomicU64::new(0),
        }
    }
}

impl Player {
    pub fn new() -> Arc<Self> {
        let p = Arc::new(Self::default());
        Self::spawn_watcher(Arc::downgrade(&p));
        p
    }

    /// Background thread that advances the playlist when a sound ends by
    /// itself. Exits when the player is dropped.
    fn spawn_watcher(weak: Weak<Self>) {
        let _ = std::thread::Builder::new().name("playlist".into()).spawn(move || {
            let mut seen = None;
            loop {
                std::thread::sleep(Duration::from_millis(15));
                let Some(p) = weak.upgrade() else { return };
                let Some(st) = p.primary_status() else {
                    seen = None;
                    continue;
                };
                let count = st.ended_count.load(Relaxed);
                let changed = seen.is_some_and(|c| c != count);
                seen = Some(count);
                if !changed {
                    continue;
                }
                let ended = st.ended_sound.load(Relaxed);
                let current = p.current.load(Relaxed);
                // Advance only when the track we started finished and nothing
                // else took over in the meantime.
                if p.mode() == PlaybackMode::Playlist && ended == current && st.sound_id.load(Relaxed) == 0 {
                    p.next();
                }
                p.notify();
            }
        });
    }

    fn primary_status(&self) -> Option<Arc<OutputStatus>> {
        let o = self.outputs.lock();
        o.speakers.as_ref().or(o.mic.as_ref()).map(|h| h.status.clone())
    }

    // ----- playback mode / queue -----

    pub fn mode(&self) -> PlaybackMode {
        PlaybackMode::from_u8(self.mode.load(Relaxed))
    }

    pub fn set_mode(&self, m: PlaybackMode) {
        if self.mode.swap(m.to_u8(), Relaxed) == m.to_u8() {
            return;
        }
        let looping = m == PlaybackMode::Loop;
        for h in self.outputs.lock().each() {
            h.send(Command::SetLooping(looping));
        }
        self.notify();
    }

    /// Set the order for next/previous and playlist mode.
    pub fn set_queue(&self, ids: Vec<u64>) {
        *self.queue.write() = ids;
    }

    /// The sound `offset` places away from the current one in the queue
    /// (wrapping), skipping sounds that aren't loaded.
    fn neighbor(&self, offset: isize) -> Option<u64> {
        let q = self.queue.read();
        if q.is_empty() {
            return None;
        }
        let bank = self.bank.read();
        let n = q.len() as isize;
        let cur = self.current.load(Relaxed);
        let start = match q.iter().position(|&id| id == cur) {
            Some(i) => i as isize,
            // Not in this list: next starts at the top, previous at the bottom.
            None if offset > 0 => -1,
            None => n,
        };
        (1..=n).map(|k| q[(start + offset * k).rem_euclid(n) as usize]).find(|id| bank.contains_key(id))
    }

    pub fn next(&self) -> bool {
        self.neighbor(1).is_some_and(|id| self.play(id))
    }

    /// Restart the current sound if it has played for a few seconds, otherwise
    /// go to the previous one.
    pub fn previous(&self) -> bool {
        let pb = self.playback();
        if pb.sound_id != 0 && pb.position_secs > 3.0 {
            self.seek(0.0);
            return true;
        }
        self.neighbor(-1).is_some_and(|id| self.play(id))
    }

    /// Jump the playing sound to `secs`.
    pub fn seek(&self, secs: f64) {
        // The sound we last started (the mixer may not have reported it yet).
        let id = match self.current.load(Relaxed) {
            0 => self.playback().sound_id,
            id => id,
        };
        let Some(sound) = self.bank.read().get(&id).cloned() else { return };
        let frame = (secs.max(0.0) * sound.sample_rate as f64) as usize;
        let looping = self.mode() == PlaybackMode::Loop;
        for h in self.outputs.lock().each() {
            h.send(Command::seek(id, &sound, frame, looping));
        }
        self.notify();
    }

    /// Move the playing sound by `delta` seconds (negative = back).
    pub fn seek_by(&self, delta: f64) {
        let pb = self.playback();
        if pb.sound_id != 0 && pb.sound_id == self.current.load(Relaxed) {
            self.seek((pb.position_secs + delta).clamp(0.0, pb.duration_secs));
        }
    }

    /// Called after anything changes (used to wake up the UI).
    pub fn set_on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_change.write() = Some(Box::new(f));
    }

    /// Wake the UI (callable from any thread).
    pub fn wake(&self) {
        self.notify();
    }

    fn notify(&self) {
        if let Some(f) = self.on_change.read().as_ref() {
            f();
        }
    }

    // ----- sound bank -----

    pub fn insert_sound(&self, id: u64, sound: SoundRef) {
        self.bank.write().insert(id, sound);
    }

    pub fn remove_sound(&self, id: u64) {
        self.bank.write().remove(&id);
        self.outputs.lock().each().for_each(|h| {
            h.send(Command::Stop { sound_id: id });
        });
    }

    pub fn has_sound(&self, id: u64) -> bool {
        self.bank.read().contains_key(&id)
    }

    // ----- transport -----

    /// Play a library sound. Returns `false` if it isn't loaded.
    pub fn play(&self, id: u64) -> bool {
        let Some(sound) = self.bank.read().get(&id).cloned() else {
            return false;
        };
        let exclusive = !self.overlap.load(Relaxed);
        let looping = self.mode() == PlaybackMode::Loop;
        self.paused.store(false, Relaxed);
        self.current.store(id, Relaxed);
        for h in self.outputs.lock().each() {
            h.send(Command::play_with(id, sound.clone(), exclusive, looping));
        }
        self.notify();
        true
    }

    pub fn stop_all(&self) {
        self.paused.store(false, Relaxed);
        for h in self.outputs.lock().each() {
            h.send(Command::StopAll);
        }
        self.notify();
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Relaxed);
        for h in self.outputs.lock().each() {
            h.send(Command::SetPaused(paused));
        }
        self.notify();
    }

    pub fn toggle_pause(&self) {
        if self.playback().sound_id != 0 {
            self.set_paused(!self.paused.load(Relaxed));
        }
    }

    // ----- volumes / routing -----

    pub fn set_output_enabled(&self, k: OutputKind, on: bool) {
        match k {
            OutputKind::Speakers => self.speakers_enabled.store(on, Relaxed),
            OutputKind::VirtualMic => self.mic_enabled.store(on, Relaxed),
        }
        self.apply_params();
    }

    pub fn output_enabled(&self, k: OutputKind) -> bool {
        match k {
            OutputKind::Speakers => self.speakers_enabled.load(Relaxed),
            OutputKind::VirtualMic => self.mic_enabled.load(Relaxed),
        }
    }

    pub fn set_volume(&self, k: OutputKind, v: f32) {
        match k {
            OutputKind::Speakers => self.speaker_volume.set(v),
            OutputKind::VirtualMic => self.mic_volume.set(v),
        }
        self.apply_params();
    }

    pub fn set_passthrough_volume(&self, v: f32) {
        self.passthrough_volume.set(v);
        self.apply_params();
    }

    fn apply_params(&self) {
        let mut o = self.outputs.lock();
        if let Some(h) = o.speakers.as_mut() {
            let on = self.speakers_enabled.load(Relaxed);
            h.params.sound_gain.set(if on { gain_curve(self.speaker_volume.get()) } else { 0.0 });
            h.params.mic_gain.set(0.0);
        }
        if let Some(h) = o.mic.as_mut() {
            // Muting the virtual mic silences it completely: sounds and your voice.
            let on = self.mic_enabled.load(Relaxed);
            h.params.sound_gain.set(if on { gain_curve(self.mic_volume.get()) } else { 0.0 });
            h.params.mic_gain.set(if on { gain_curve(self.passthrough_volume.get()) } else { 0.0 });
        }
    }

    // ----- output wiring (called by the engine) -----

    pub(crate) fn install_output(&self, k: OutputKind, h: Option<MixerHandle>) {
        let mut h = h;
        if let Some(h) = h.as_mut()
            && self.paused.load(Relaxed)
        {
            h.send(Command::SetPaused(true));
        }
        let old = std::mem::replace(self.outputs.lock().get(k), h);
        drop(old);
        self.apply_params();
    }

    pub(crate) fn set_mic_input(&self, mic: Option<Box<MicInput>>) {
        if let Some(h) = self.outputs.lock().mic.as_mut() {
            h.send(Command::SetMicInput(mic));
        }
    }

    pub fn output_status(&self, k: OutputKind) -> Option<Arc<OutputStatus>> {
        self.outputs.lock().get(k).as_ref().map(|h| h.status.clone())
    }

    /// Free memory released by the audio threads. Call periodically.
    pub fn collect_garbage(&self) {
        self.outputs.lock().each().for_each(|h| h.collect_garbage());
    }

    pub fn playback(&self) -> PlaybackInfo {
        let o = self.outputs.lock();
        let st = o.speakers.as_ref().or(o.mic.as_ref()).map(|h| &h.status);
        let Some(st) = st else { return PlaybackInfo::default() };
        let rate = st.source_rate.load(Relaxed).max(1) as f64;
        let peak = [o.speakers.as_ref(), o.mic.as_ref()]
            .into_iter()
            .flatten()
            .map(|h| h.status.peak.get())
            .fold(0.0, f32::max);
        PlaybackInfo {
            sound_id: st.sound_id.load(Relaxed),
            position_secs: st.pos_frames.load(Relaxed) as f64 / rate,
            duration_secs: st.total_frames.load(Relaxed) as f64 / rate,
            paused: self.paused.load(Relaxed),
            active_voices: st.active_voices.load(Relaxed),
            peak,
        }
    }
}

/// Map a 0..1 slider (up to 2.0 for boost) to a perceptually even gain.
pub fn gain_curve(v: f32) -> f32 {
    let v = v.max(0.0);
    v * v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode::SoundData;
    use crate::audio::mixer::{Mixer, mixer};

    fn sound(frames: usize) -> SoundRef {
        Arc::new(SoundData::new(vec![0.25; frames * 2], 48_000))
    }

    fn setup() -> (Arc<Player>, Mixer) {
        let p = Player::new();
        let (m, h) = mixer(48_000);
        p.install_output(OutputKind::Speakers, Some(h));
        for id in 1..=3 {
            p.insert_sound(id, sound(4800));
        }
        p.set_queue(vec![1, 2, 3]);
        (p, m)
    }

    fn render(m: &mut Mixer, frames: usize) {
        let mut out = vec![0.0; frames * 2];
        m.render(&mut out, 2);
    }

    #[test]
    fn muting_an_output_silences_sounds_and_voice() {
        let p = Player::new();
        let (_sm, sh) = mixer(48_000);
        let (_mm, mh) = mixer(48_000);
        let (speakers, mic) = (sh.params.clone(), mh.params.clone());
        p.install_output(OutputKind::Speakers, Some(sh));
        p.install_output(OutputKind::VirtualMic, Some(mh));
        assert!(mic.sound_gain.get() > 0.0 && mic.mic_gain.get() > 0.0);

        p.set_output_enabled(OutputKind::VirtualMic, false);
        assert_eq!((mic.sound_gain.get(), mic.mic_gain.get()), (0.0, 0.0), "virtual mic fully muted");
        assert!(speakers.sound_gain.get() > 0.0, "speakers unaffected");

        p.set_output_enabled(OutputKind::Speakers, false);
        assert_eq!((speakers.sound_gain.get(), speakers.mic_gain.get()), (0.0, 0.0), "speakers fully muted");

        p.set_output_enabled(OutputKind::VirtualMic, true);
        assert!(mic.sound_gain.get() > 0.0 && mic.mic_gain.get() > 0.0, "unmute restores voice");
    }

    #[test]
    fn next_and_previous_follow_the_queue_and_wrap() {
        let (p, mut m) = setup();
        assert!(p.play(3));
        render(&mut m, 64);
        assert!(p.next());
        render(&mut m, 64);
        assert_eq!(p.playback().sound_id, 1, "wraps to the top");
        assert!(p.previous());
        render(&mut m, 64);
        assert_eq!(p.playback().sound_id, 3, "wraps to the bottom");
    }

    #[test]
    fn playlist_mode_advances_when_a_sound_ends() {
        let (p, mut m) = setup();
        p.set_mode(PlaybackMode::Playlist);
        p.play(1);
        // Render past the end of sound 1, then give the watcher time to react.
        let mut next = 0;
        for _ in 0..200 {
            render(&mut m, 256);
            std::thread::sleep(Duration::from_millis(2));
            next = p.playback().sound_id;
            if next == 2 {
                break;
            }
        }
        assert_eq!(next, 2);
    }

    #[test]
    fn once_mode_stops_at_the_end() {
        let (p, mut m) = setup();
        p.play(1);
        for _ in 0..40 {
            render(&mut m, 256);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(p.playback().sound_id, 0);
    }
}

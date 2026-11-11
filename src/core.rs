//! UI-independent application core: library, settings, devices, hotkeys and the
//! virtual mic driver. The native SwiftUI front end drives it through `ffi`.
//!
//! All methods must be called from the main thread (global hotkeys are
//! registered there). Background work (decoding, driver install) reports back
//! through `tick()`; `wake` is invoked from any thread when there is news.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::audio::decode;
use crate::audio::engine::{self, AudioEngine, DeviceInfo};
use crate::audio::player::{OutputKind, PlaybackInfo, PlaybackMode, Player};
use crate::hotkeys::{self, Action, Hotkeys};
use crate::library::{self, Loader, SavedPlaylist, SavedSound, SavedState, Settings};
use crate::virtual_mic;

pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoadState {
    Loading,
    Ready {
        duration: f64,
    },
    Failed {
        error: String,
    },
    /// The file isn't there any more (moved or deleted).
    Missing,
}

struct PlaylistEntry {
    id: u64,
    name: String,
    sounds: Vec<u64>,
}

struct SoundEntry {
    id: u64,
    name: String,
    path: PathBuf,
    hotkey: Option<String>,
    added: Option<u64>,
    state: LoadState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyTarget {
    Sound(u64),
    Stop,
    Pause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Speakers,
    Mic,
}

impl From<Output> for OutputKind {
    fn from(o: Output) -> Self {
        match o {
            Output::Speakers => OutputKind::Speakers,
            Output::Mic => OutputKind::VirtualMic,
        }
    }
}

/// Everything the UI can ask for.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Add files/folders to the library (and to `playlist`, if given).
    AddFiles {
        paths: Vec<PathBuf>,
        #[serde(default)]
        playlist: Option<u64>,
    },
    Remove {
        id: u64,
    },
    Play {
        id: u64,
    },
    Stop,
    TogglePause,
    Next,
    Previous,
    /// Jump to an absolute position of the playing sound.
    Seek {
        secs: f64,
    },
    /// Move the playing sound by `secs` (negative = back).
    SeekBy {
        secs: f64,
    },
    SetPlaybackMode {
        mode: PlaybackMode,
    },
    /// Creates a playlist (optionally with sounds) and makes it the active one.
    CreatePlaylist {
        name: String,
        #[serde(default)]
        ids: Vec<u64>,
    },
    RenamePlaylist {
        id: u64,
        name: String,
    },
    DeletePlaylist {
        id: u64,
    },
    AddToPlaylist {
        playlist: u64,
        ids: Vec<u64>,
    },
    RemoveFromPlaylist {
        playlist: u64,
        ids: Vec<u64>,
    },
    MoveInPlaylist {
        playlist: u64,
        id: u64,
        to_index: usize,
    },
    /// List shown and used for next/previous (`None` = all sounds).
    SetActivePlaylist {
        id: Option<u64>,
    },
    Rename {
        id: u64,
        name: String,
    },
    Move {
        id: u64,
        to_index: usize,
    },
    Retry {
        id: u64,
    },
    Reveal {
        id: u64,
    },
    /// Release global hotkeys while the UI records a new combination.
    BeginCapture,
    EndCapture,
    SetHotkey {
        target: HotkeyTarget,
        hotkey: Option<String>,
    },
    SetOutputEnabled {
        output: Output,
        on: bool,
    },
    SetVolume {
        output: Output,
        value: f32,
    },
    SetPassthroughVolume {
        value: f32,
    },
    SetOverlap {
        on: bool,
    },
    /// Allow the virtual-mic volume up to 150%. Turning it off caps it at 100%.
    SetMicBoost {
        on: bool,
    },
    /// `None` = system default.
    SetSpeakerDevice {
        id: Option<String>,
    },
    /// `None` = off.
    SetMicDevice {
        id: Option<String>,
    },
    /// `None` = off, `Some("")` = system default input.
    SetInputDevice {
        id: Option<String>,
    },
    RefreshDevices,
    InstallDriver,
    UninstallDriver,
    UseVirtualMic,
    OpenLibraryFolder,
}

#[derive(Serialize)]
pub struct SoundView {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub hotkey_label: Option<String>,
    /// Seconds since 1970; absent for sounds added before dates were recorded.
    pub added: Option<u64>,
    #[serde(flatten)]
    pub state: LoadState,
}

#[derive(Serialize)]
pub struct PlaylistView {
    pub id: u64,
    pub name: String,
    pub sounds: Vec<u64>,
}

#[derive(Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub is_virtual: bool,
    pub is_soundpad: bool,
}

impl From<&DeviceInfo> for DeviceView {
    fn from(d: &DeviceInfo) -> Self {
        Self {
            id: d.id.clone(),
            // In Soundpad, the Feed is simply "Soundpad Virtual Mic".
            name: if d.name == virtual_mic::FEED_NAME { virtual_mic::DEVICE_NAME.to_string() } else { d.name.clone() },
            is_virtual: d.looks_virtual(),
            is_soundpad: virtual_mic::is_virtual_mic(d),
        }
    }
}

#[derive(Serialize)]
pub struct OutputDiag {
    pub output: Output,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: u64,
    pub dsp_load_percent: f64,
    pub dsp_peak_percent: f64,
    pub mic_underruns: u64,
}

#[derive(Serialize)]
pub struct DriverView {
    pub bundled: bool,
    pub installed: bool,
    pub update_available: bool,
    pub busy: bool,
    pub selected: bool,
}

#[derive(Serialize)]
pub struct StateView {
    pub sounds: Vec<SoundView>,
    pub playlists: Vec<PlaylistView>,
    pub active_playlist: Option<u64>,
    pub settings: Settings,
    pub stop_hotkey_label: Option<String>,
    pub pause_hotkey_label: Option<String>,
    pub outputs: Vec<DeviceView>,
    pub inputs: Vec<DeviceView>,
    pub default_input_name: Option<String>,
    /// A loopback device such as BlackHole that could be used instead of ours.
    pub other_loopback: Option<DeviceView>,
    /// The output Soundpad plays to for "you" (may be a loopback, then you hear nothing).
    pub speaker_device_name: Option<String>,
    pub speaker_is_virtual: bool,
    pub driver: DriverView,
    pub diagnostics: Vec<OutputDiag>,
    pub input_device_name: Option<String>,
    pub config_path: String,
    /// One-shot notifications for the UI (drained by each `state()` call).
    pub messages: Vec<String>,
}

pub struct Core {
    player: Arc<Player>,
    engine: AudioEngine,
    hotkeys: Hotkeys,
    loader: Loader,
    sounds: Vec<SoundEntry>,
    next_id: u64,
    playlists: Vec<PlaylistEntry>,
    active_playlist: Option<u64>,
    settings: Settings,
    config_path: PathBuf,
    capturing: bool,
    messages: Vec<String>,
    out_devices: Vec<DeviceInfo>,
    in_devices: Vec<DeviceInfo>,
    dirty_since: Option<Instant>,
    reopen_at: Option<Instant>,
    last_default_check: Instant,
    driver_job: Option<(bool, crossbeam_channel::Receiver<Result<(), String>>)>,
    await_virtual_mic_until: Option<Instant>,
    wake: Waker,
    /// False in headless tests: no audio devices, no global hotkeys.
    hardware: bool,
}

impl Core {
    pub fn new(config_path: PathBuf, wake: Waker) -> Self {
        Self::build(config_path, wake, true)
    }

    /// The same core without audio devices or global hotkeys, for tests.
    #[cfg(test)]
    fn headless(config_path: PathBuf) -> Self {
        Self::build(config_path, Arc::new(|| {}), false)
    }

    fn build(config_path: PathBuf, wake: Waker, hardware: bool) -> Self {
        let state = library::load_from(&config_path);
        let player = Player::new();
        {
            let wake = wake.clone();
            player.set_on_change(move || wake());
        }
        let loader = Loader::new(wake.clone());
        let hotkeys = if hardware { Hotkeys::new(player.clone()) } else { Hotkeys::disabled() };
        let engine = AudioEngine::new(player.clone());
        let mut core = Self {
            player,
            engine,
            hotkeys,
            loader,
            sounds: Vec::new(),
            next_id: 1,
            playlists: Vec::new(),
            active_playlist: None,
            settings: state.settings,
            config_path,
            capturing: false,
            messages: Vec::new(),
            out_devices: Vec::new(),
            in_devices: Vec::new(),
            dirty_since: None,
            reopen_at: None,
            last_default_check: Instant::now(),
            driver_job: None,
            await_virtual_mic_until: None,
            wake,
            hardware,
        };
        if let Some(e) = core.hotkeys.init_error.clone() {
            core.notify(e);
        }
        for s in state.sounds {
            core.push_sound(s.path, s.name, s.hotkey, s.added);
        }
        let missing = core.sounds.iter().filter(|s| matches!(s.state, LoadState::Missing)).count();
        if missing > 0 {
            core.notify(format!("{missing} sound file(s) are missing"));
        }
        for pl in state.playlists {
            let sounds =
                pl.sounds.iter().filter_map(|p| core.sounds.iter().find(|s| &s.path == p)).map(|s| s.id).collect();
            let id = core.alloc_id();
            core.playlists.push(PlaylistEntry { id, name: pl.name, sounds });
        }
        core.sync_queue();
        core.apply_player_settings();
        if hardware {
            core.refresh_device_lists();
            core.open_devices();
        }
        core.sync_hotkeys();
        core
    }

    pub fn player(&self) -> &Arc<Player> {
        &self.player
    }

    pub fn playback(&self) -> PlaybackInfo {
        self.player.playback()
    }

    // ---------- commands ----------

    pub fn command(&mut self, cmd: Command) -> Result<(), String> {
        let r = self.command_inner(cmd);
        self.sync_queue();
        r
    }

    fn command_inner(&mut self, cmd: Command) -> Result<(), String> {
        use Command as C;
        match cmd {
            C::AddFiles { paths, playlist } => {
                let ids = self.add_files(paths);
                if let Some(pl) = playlist {
                    self.add_to_playlist(pl, &ids)?;
                }
            }
            C::Next => {
                if !self.player.next() {
                    return Err("Nothing to play".into());
                }
            }
            C::Previous => {
                if !self.player.previous() {
                    return Err("Nothing to play".into());
                }
            }
            C::Seek { secs } => self.player.seek(secs),
            C::SeekBy { secs } => self.player.seek_by(secs),
            C::SetPlaybackMode { mode } => {
                self.settings.playback_mode = mode;
                self.player.set_mode(mode);
                self.mark_dirty();
            }
            C::CreatePlaylist { name, ids } => {
                let name = self.unique_playlist_name(name.trim());
                let id = self.alloc_id();
                let mut sounds = Vec::new();
                for i in ids {
                    if self.index_of(i).is_some() && !sounds.contains(&i) {
                        sounds.push(i);
                    }
                }
                self.playlists.push(PlaylistEntry { id, name, sounds });
                self.active_playlist = Some(id);
                self.mark_dirty();
            }
            C::RenamePlaylist { id, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err("Name can't be empty".into());
                }
                self.playlist_mut(id)?.name = name;
                self.mark_dirty();
            }
            C::DeletePlaylist { id } => {
                self.playlists.retain(|p| p.id != id);
                if self.active_playlist == Some(id) {
                    self.active_playlist = None;
                }
                self.mark_dirty();
            }
            C::AddToPlaylist { playlist, ids } => self.add_to_playlist(playlist, &ids)?,
            C::RemoveFromPlaylist { playlist, ids } => {
                self.playlist_mut(playlist)?.sounds.retain(|s| !ids.contains(s));
                self.mark_dirty();
            }
            C::MoveInPlaylist { playlist, id, to_index } => {
                let pl = self.playlist_mut(playlist)?;
                let i = pl.sounds.iter().position(|&s| s == id).ok_or("not in playlist")?;
                let e = pl.sounds.remove(i);
                let to = to_index.min(pl.sounds.len());
                pl.sounds.insert(to, e);
                self.mark_dirty();
            }
            C::SetActivePlaylist { id } => {
                if let Some(id) = id {
                    self.playlist_mut(id)?;
                }
                self.active_playlist = id;
            }
            C::Remove { id } => self.remove(id),
            C::Play { id } => return self.play(id),
            C::Stop => self.player.stop_all(),
            C::TogglePause => self.player.toggle_pause(),
            C::Rename { id, name } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err("Name can't be empty".into());
                }
                let i = self.index_of(id).ok_or("unknown sound")?;
                self.sounds[i].name = name;
                self.mark_dirty();
            }
            C::Move { id, to_index } => {
                let i = self.index_of(id).ok_or("unknown sound")?;
                let e = self.sounds.remove(i);
                let to = to_index.min(self.sounds.len());
                self.sounds.insert(to, e);
                self.mark_dirty();
            }
            C::Retry { id } => {
                let i = self.index_of(id).ok_or("unknown sound")?;
                self.sounds[i].state = self.start_loading(id, self.sounds[i].path.clone());
            }
            C::Reveal { id } => {
                let i = self.index_of(id).ok_or("unknown sound")?;
                let _ = std::process::Command::new("open").arg("-R").arg(&self.sounds[i].path).spawn();
            }
            C::BeginCapture => {
                self.capturing = true;
                self.sync_hotkeys();
            }
            C::EndCapture => {
                self.capturing = false;
                self.sync_hotkeys();
            }
            C::SetHotkey { target, hotkey } => {
                let hotkey = match hotkey {
                    Some(h) => Some(hotkeys::validate_capture(&h)?),
                    None => None,
                };
                self.set_hotkey(target, hotkey);
                self.sync_hotkeys();
            }
            C::SetOutputEnabled { output, on } => {
                match output {
                    Output::Speakers => self.settings.speakers_enabled = on,
                    Output::Mic => self.settings.mic_enabled = on,
                }
                self.apply_player_settings();
                self.mark_dirty();
            }
            C::SetVolume { output, value } => {
                match output {
                    Output::Speakers => self.settings.speaker_volume = value.clamp(0.0, 1.0),
                    Output::Mic => self.settings.mic_volume = value.clamp(0.0, self.max_mic_volume()),
                }
                self.apply_player_settings();
                self.mark_dirty();
            }
            C::SetPassthroughVolume { value } => {
                self.settings.passthrough_volume = value.clamp(0.0, 2.0);
                self.apply_player_settings();
                self.mark_dirty();
            }
            C::SetMicBoost { on } => {
                self.settings.mic_boost = on;
                self.settings.mic_volume = self.settings.mic_volume.min(self.max_mic_volume());
                self.apply_player_settings();
                self.mark_dirty();
            }
            C::SetOverlap { on } => {
                self.settings.overlap = on;
                self.apply_player_settings();
                self.mark_dirty();
            }
            C::SetSpeakerDevice { id } => {
                self.settings.speaker_device = id.clone();
                self.mark_dirty();
                self.engine.set_output(OutputKind::Speakers, Some(id.as_deref().unwrap_or("")))?;
            }
            C::SetMicDevice { id } => {
                self.settings.mic_device = id.clone();
                self.settings.mic_device_chosen = true;
                self.mark_dirty();
                self.engine.set_output(OutputKind::VirtualMic, id.as_deref())?;
            }
            C::SetInputDevice { id } => {
                self.settings.input_device = id.clone();
                self.mark_dirty();
                self.refresh_device_lists();
                self.engine.set_input(id.as_deref())?;
            }
            C::RefreshDevices => self.refresh_device_lists(),
            C::InstallDriver => self.start_driver_job(true),
            C::UninstallDriver => self.start_driver_job(false),
            C::UseVirtualMic => {
                if !self.select_virtual_mic() {
                    return Err("Soundpad Virtual Mic is not available yet".into());
                }
            }
            C::OpenLibraryFolder => {
                let _ = std::fs::create_dir_all(library::config_dir());
                let _ = std::process::Command::new("open").arg(library::config_dir()).spawn();
            }
        }
        Ok(())
    }

    // ---------- state for the UI ----------

    pub fn state(&mut self) -> StateView {
        let label = |h: &Option<String>| h.as_deref().map(hotkeys::pretty);
        let virtual_selected = self
            .settings
            .mic_device
            .as_ref()
            .is_some_and(|id| self.out_devices.iter().any(|d| &d.id == id && virtual_mic::is_virtual_mic(d)));
        let default_input_name = if self.in_devices.is_empty() {
            None
        } else {
            engine::pick_default_input(engine::default_input_id().as_deref(), &self.in_devices)
                .and_then(|id| self.in_devices.iter().find(|d| d.id == id).map(|d| d.name.clone()))
        };
        let speaker = self.engine.output(OutputKind::Speakers).map(|o| &o.device);
        StateView {
            playlists: self
                .playlists
                .iter()
                .map(|p| PlaylistView { id: p.id, name: p.name.clone(), sounds: p.sounds.clone() })
                .collect(),
            active_playlist: self.active_playlist,
            sounds: self
                .sounds
                .iter()
                .map(|s| SoundView {
                    id: s.id,
                    name: s.name.clone(),
                    path: s.path.display().to_string(),
                    hotkey: s.hotkey.clone(),
                    hotkey_label: label(&s.hotkey),
                    added: s.added,
                    state: s.state.clone(),
                })
                .collect(),
            stop_hotkey_label: label(&self.settings.stop_hotkey),
            pause_hotkey_label: label(&self.settings.pause_hotkey),
            settings: self.settings.clone(),
            outputs: self.out_devices.iter().map(DeviceView::from).collect(),
            inputs: self.in_devices.iter().map(DeviceView::from).collect(),
            default_input_name,
            other_loopback: virtual_mic::other_loopback(&self.out_devices).map(DeviceView::from),
            speaker_device_name: speaker.map(|d| d.name.clone()),
            speaker_is_virtual: speaker.is_some_and(|d| d.looks_virtual()),
            driver: DriverView {
                bundled: virtual_mic::bundled_driver().is_some(),
                installed: virtual_mic::is_installed(),
                update_available: virtual_mic::update_available(),
                busy: self.driver_job.is_some() || self.await_virtual_mic_until.is_some(),
                selected: virtual_selected,
            },
            diagnostics: self.diagnostics(),
            input_device_name: self.engine.input_device().map(|d| d.name.clone()),
            config_path: self.config_path.display().to_string(),
            messages: std::mem::take(&mut self.messages),
        }
    }

    fn diagnostics(&self) -> Vec<OutputDiag> {
        let mut v = Vec::new();
        for (o, k) in [(Output::Speakers, OutputKind::Speakers), (Output::Mic, OutputKind::VirtualMic)] {
            if let (Some(out), Some(st)) = (self.engine.output(k), self.player.output_status(k)) {
                let frames = st.callback_frames.load(Relaxed).max(1);
                let period_ns = frames as f64 / out.sample_rate as f64 * 1e9;
                v.push(OutputDiag {
                    output: o,
                    device: out.device.name.clone(),
                    sample_rate: out.sample_rate,
                    channels: out.channels,
                    buffer_frames: frames,
                    dsp_load_percent: st.last_callback_ns.load(Relaxed) as f64 / period_ns * 100.0,
                    dsp_peak_percent: st.max_callback_ns.load(Relaxed) as f64 / period_ns * 100.0,
                    mic_underruns: st.mic_underruns.load(Relaxed),
                });
            }
        }
        v
    }

    // ---------- periodic work ----------

    /// Call regularly (e.g. 4×/s) from the main thread. Returns true if the
    /// UI should refresh its state.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.poll_loader();
        self.player.collect_garbage();
        changed |= self.poll_driver_job();

        if let Some(deadline) = self.await_virtual_mic_until {
            if self.select_virtual_mic() {
                self.await_virtual_mic_until = None;
                self.notify("Soundpad Virtual Mic is ready — choose it as the microphone in your chat app");
            } else if Instant::now() > deadline {
                self.await_virtual_mic_until = None;
                self.notify("Driver installed, but the device didn't appear. Try restarting your Mac.");
            }
            changed = true;
        }
        if self.engine.take_device_lost() {
            self.reopen_at = Some(Instant::now() + Duration::from_millis(500));
        }
        if self.reopen_at.is_some_and(|t| Instant::now() >= t) {
            self.reopen_at = None;
            self.refresh_device_lists();
            self.open_devices();
            changed = true;
        }
        if self.last_default_check.elapsed() > Duration::from_secs(2) {
            self.last_default_check = Instant::now();
            changed |= self.check_missing_files();
            if self.hardware && self.settings.speaker_device.is_none() {
                let current = self.engine.output(OutputKind::Speakers).map(|o| o.device.id.clone());
                let default = engine::default_output_id();
                if default.is_some() && current != default {
                    let _ = self.engine.set_output(OutputKind::Speakers, Some(""));
                    changed = true;
                }
            }
        }
        if self.dirty_since.is_some_and(|t| t.elapsed() > Duration::from_millis(600)) {
            self.save();
        }
        changed || !self.messages.is_empty()
    }

    pub fn save(&mut self) {
        let snapshot = SavedState {
            sounds: self
                .sounds
                .iter()
                .map(|s| SavedSound {
                    path: s.path.clone(),
                    name: s.name.clone(),
                    hotkey: s.hotkey.clone(),
                    added: s.added,
                })
                .collect(),
            settings: self.settings.clone(),
            playlists: self
                .playlists
                .iter()
                .map(|p| SavedPlaylist {
                    name: p.name.clone(),
                    sounds: p
                        .sounds
                        .iter()
                        .filter_map(|&id| self.index_of(id))
                        .map(|i| self.sounds[i].path.clone())
                        .collect(),
                })
                .collect(),
        };
        if let Err(e) = library::save_to(&self.config_path, &snapshot) {
            self.notify(format!("Could not save library: {e}"));
        }
        self.dirty_since = None;
    }

    // ---------- internals ----------

    fn notify(&mut self, msg: impl Into<String>) {
        self.messages.push(msg.into());
        (self.wake)();
    }

    fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.sounds.iter().position(|s| s.id == id)
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn playlist_mut(&mut self, id: u64) -> Result<&mut PlaylistEntry, String> {
        self.playlists.iter_mut().find(|p| p.id == id).ok_or_else(|| "unknown playlist".to_string())
    }

    fn add_to_playlist(&mut self, playlist: u64, ids: &[u64]) -> Result<(), String> {
        let valid: Vec<u64> = ids.iter().copied().filter(|&i| self.index_of(i).is_some()).collect();
        let pl = self.playlist_mut(playlist)?;
        for i in valid {
            if !pl.sounds.contains(&i) {
                pl.sounds.push(i);
            }
        }
        self.mark_dirty();
        Ok(())
    }

    /// `base`, or `base 2`, `base 3`… if taken (empty = "New Playlist").
    fn unique_playlist_name(&self, base: &str) -> String {
        let base = if base.is_empty() { "New Playlist" } else { base };
        (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") })
            .find(|n| !self.playlists.iter().any(|p| &p.name == n))
            .unwrap()
    }

    /// Highest virtual-mic volume: 150% with the boost setting, else 100%.
    fn max_mic_volume(&self) -> f32 {
        if self.settings.mic_boost { 1.5 } else { 1.0 }
    }

    /// Tell the player the order used by next/previous and playlist mode.
    fn sync_queue(&self) {
        let ids = match self.active_playlist.and_then(|id| self.playlists.iter().find(|p| p.id == id)) {
            Some(p) => p.sounds.clone(),
            None => self.sounds.iter().map(|s| s.id).collect(),
        };
        self.player.set_queue(ids);
    }

    fn push_sound(&mut self, path: PathBuf, name: String, hotkey: Option<String>, added: Option<u64>) -> u64 {
        let id = self.alloc_id();
        let state = self.start_loading(id, path.clone());
        self.sounds.push(SoundEntry { id, name, path, hotkey, added, state });
        id
    }

    /// Adds new files to the library; returns the ids of all given files
    /// (including ones that were already in the library), in order.
    fn add_files(&mut self, paths: Vec<PathBuf>) -> Vec<u64> {
        let mut ids = Vec::new();
        let mut added = 0;
        let mut skipped = 0;
        let mut files = Vec::new();
        for p in paths {
            if p.is_dir() {
                let mut inner: Vec<PathBuf> = std::fs::read_dir(&p)
                    .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
                    .unwrap_or_default();
                inner.sort();
                files.extend(inner.into_iter().filter(|f| f.is_file() && decode::is_supported(f)));
            } else if decode::is_supported(&p) {
                files.push(p);
            } else {
                skipped += 1;
            }
        }
        for f in files {
            match self.sounds.iter().find(|s| s.path == f) {
                Some(s) => ids.push(s.id),
                None => {
                    let name = library::display_name(&f);
                    ids.push(self.push_sound(f, name, None, Some(library::now_secs())));
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.mark_dirty();
        }
        if skipped > 0 {
            self.notify(format!("Skipped {skipped} unsupported file(s)"));
        }
        ids
    }

    fn remove(&mut self, id: u64) {
        if let Some(i) = self.index_of(id) {
            self.sounds.remove(i);
            self.player.remove_sound(id);
            for p in &mut self.playlists {
                p.sounds.retain(|&s| s != id);
            }
            self.mark_dirty();
            self.sync_hotkeys();
        }
    }

    /// Marks sounds whose file was moved or deleted as missing (so hotkeys
    /// don't play a few buffered seconds), and reloads ones that came back.
    fn check_missing_files(&mut self) -> bool {
        let mut gone = 0;
        let mut changed = false;
        for i in 0..self.sounds.len() {
            let (id, exists) = (self.sounds[i].id, self.sounds[i].path.exists());
            match self.sounds[i].state {
                LoadState::Missing if exists => {
                    self.sounds[i].state = self.start_loading(id, self.sounds[i].path.clone());
                    changed = true;
                }
                LoadState::Ready { .. } if !exists => {
                    self.sounds[i].state = LoadState::Missing;
                    self.player.remove_sound(id);
                    gone += 1;
                    changed = true;
                }
                _ => {}
            }
        }
        if gone > 0 {
            self.notify(format!("{gone} sound file(s) are missing"));
        }
        changed
    }

    /// Starts decoding a file, or reports it missing without trying.
    fn start_loading(&self, id: u64, path: PathBuf) -> LoadState {
        if path.exists() {
            self.loader.load(id, path);
            LoadState::Loading
        } else {
            LoadState::Missing
        }
    }

    fn play(&mut self, id: u64) -> Result<(), String> {
        let i = self.index_of(id).ok_or("unknown sound")?;
        // Long sounds stream from disk: a file deleted while Soundpad runs would
        // play only what is already buffered, so check before playing.
        if !matches!(self.sounds[i].state, LoadState::Missing) && !self.sounds[i].path.exists() {
            self.sounds[i].state = LoadState::Missing;
            self.player.remove_sound(id);
        }
        match &self.sounds[i].state {
            LoadState::Ready { .. } => {
                self.player.play(id);
                Ok(())
            }
            LoadState::Loading => Err("Still loading…".into()),
            LoadState::Failed { error } => Err(format!("Can't play “{}”: {error}", self.sounds[i].name)),
            LoadState::Missing => Err(format!("“{}” is missing. Was it moved or deleted?", self.sounds[i].name)),
        }
    }

    fn poll_loader(&mut self) -> bool {
        let mut changed = false;
        while let Ok((id, result)) = self.loader.results.try_recv() {
            let Some(i) = self.index_of(id) else { continue };
            self.sounds[i].state = match result {
                Ok(sound) => {
                    let duration = sound.duration_secs();
                    self.player.insert_sound(id, sound);
                    LoadState::Ready { duration }
                }
                Err(error) => LoadState::Failed { error },
            };
            changed = true;
        }
        changed
    }

    fn apply_player_settings(&self) {
        let s = &self.settings;
        self.player.overlap.store(s.overlap, Relaxed);
        self.player.set_mode(s.playback_mode);
        self.player.set_output_enabled(OutputKind::Speakers, s.speakers_enabled);
        self.player.set_output_enabled(OutputKind::VirtualMic, s.mic_enabled);
        self.player.set_volume(OutputKind::Speakers, s.speaker_volume);
        self.player.set_volume(OutputKind::VirtualMic, s.mic_volume);
        self.player.set_passthrough_volume(s.passthrough_volume);
        // Testing aid: render everything but keep it inaudible.
        if std::env::var_os("SOUNDPAD_MUTE").is_some() {
            self.player.set_volume(OutputKind::Speakers, 0.0);
            self.player.set_volume(OutputKind::VirtualMic, 0.0);
            self.player.set_passthrough_volume(0.0);
        }
    }

    fn open_devices(&mut self) {
        // After the driver update, a saved choice of the old two-way device moves
        // to the Feed (same driver, playback side).
        if let Some(id) = &self.settings.mic_device
            && id.contains(virtual_mic::DEVICE_NAME)
            && !self.out_devices.iter().any(|d| &d.id == id)
            && let Some(feed) = virtual_mic::auto_mic_device(&self.out_devices)
        {
            self.settings.mic_device = Some(feed);
            self.mark_dirty();
        }
        if !self.settings.mic_device_chosen {
            // Never keep a loopback the user didn't pick (older versions auto-selected
            // BlackHole & co., which leaks sounds into apps like OBS).
            let pick = virtual_mic::auto_mic_device(&self.out_devices);
            if self.settings.mic_device != pick {
                self.settings.mic_device = pick;
                self.mark_dirty();
            }
        }
        let speaker = self.settings.speaker_device.clone().unwrap_or_default();
        if let Err(e) = self.engine.set_output(OutputKind::Speakers, Some(&speaker)) {
            if !speaker.is_empty() {
                let _ = self.engine.set_output(OutputKind::Speakers, Some(""));
            }
            self.notify(e);
        }
        let mic = self.settings.mic_device.clone();
        if let Err(e) = self.engine.set_output(OutputKind::VirtualMic, mic.as_deref()) {
            self.notify(e);
        }
        let input = self.settings.input_device.clone();
        if let Err(e) = self.engine.set_input(input.as_deref()) {
            self.notify(e);
        }
    }

    fn refresh_device_lists(&mut self) {
        self.out_devices = engine::output_devices();
        // Looking at microphones can make macOS ask for microphone permission, so
        // only do it once the user has turned on "Mix in my microphone".
        self.in_devices = if self.settings.input_device.is_some() { engine::input_devices() } else { Vec::new() };
    }

    fn start_driver_job(&mut self, install: bool) {
        if self.driver_job.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let r = if install { virtual_mic::install() } else { virtual_mic::uninstall() };
            let _ = tx.send(r);
            wake();
        });
        self.driver_job = Some((install, rx));
        if !install {
            let is_ours = self
                .settings
                .mic_device
                .as_ref()
                .is_some_and(|id| self.out_devices.iter().any(|d| &d.id == id && virtual_mic::is_virtual_mic(d)));
            if is_ours {
                self.settings.mic_device = None;
                self.settings.mic_device_chosen = true;
                let _ = self.engine.set_output(OutputKind::VirtualMic, None);
                self.mark_dirty();
            }
        }
    }

    fn select_virtual_mic(&mut self) -> bool {
        self.refresh_device_lists();
        let Some(id) = virtual_mic::find_device(&self.out_devices).map(|d| d.id.clone()) else { return false };
        self.settings.mic_device = Some(id.clone());
        self.settings.mic_device_chosen = true;
        if let Err(e) = self.engine.set_output(OutputKind::VirtualMic, Some(&id)) {
            self.notify(e);
        }
        self.mark_dirty();
        true
    }

    fn poll_driver_job(&mut self) -> bool {
        let Some((install, rx)) = &self.driver_job else { return false };
        let install = *install;
        let Ok(result) = rx.try_recv() else { return false };
        self.driver_job = None;
        match result {
            Ok(()) if install => {
                self.await_virtual_mic_until = Some(Instant::now() + Duration::from_secs(15));
            }
            Ok(()) => self.notify("Soundpad Virtual Mic removed"),
            Err(e) if e == "Cancelled" => {}
            Err(e) => self.notify(format!("Virtual mic setup failed: {e}")),
        }
        self.reopen_at = Some(Instant::now() + Duration::from_millis(800));
        true
    }

    fn sync_hotkeys(&mut self) {
        let mut wanted = Vec::new();
        if !self.capturing {
            for s in &self.sounds {
                if let Some(h) = &s.hotkey {
                    wanted.push((h.clone(), Action::Play(s.id)));
                }
            }
            if let Some(h) = &self.settings.stop_hotkey {
                wanted.push((h.clone(), Action::Stop));
            }
            if let Some(h) = &self.settings.pause_hotkey {
                wanted.push((h.clone(), Action::TogglePause));
            }
        }
        let errors = self.hotkeys.sync(&wanted);
        if let Some((k, e)) = errors.first() {
            self.notify(format!("Hotkey {} failed: {e}", hotkeys::pretty(k)));
        }
    }

    fn set_hotkey(&mut self, t: HotkeyTarget, hk: Option<String>) {
        if let Some(h) = &hk {
            // One combination does one thing: take it away from others.
            for s in &mut self.sounds {
                if s.hotkey.as_ref() == Some(h) {
                    s.hotkey = None;
                }
            }
            if self.settings.stop_hotkey.as_ref() == Some(h) {
                self.settings.stop_hotkey = None;
            }
            if self.settings.pause_hotkey.as_ref() == Some(h) {
                self.settings.pause_hotkey = None;
            }
        }
        match t {
            HotkeyTarget::Sound(id) => {
                if let Some(i) = self.index_of(id) {
                    self.sounds[i].hotkey = hk;
                }
            }
            HotkeyTarget::Stop => self.settings.stop_hotkey = hk,
            HotkeyTarget::Pause => self.settings.pause_hotkey = hk,
        }
        self.mark_dirty();
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if self.dirty_since.is_some() {
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_from_json() {
        let c: Command = serde_json::from_str(r#"{"cmd":"play","id":3}"#).unwrap();
        assert!(matches!(c, Command::Play { id: 3 }));
        let c: Command =
            serde_json::from_str(r#"{"cmd":"set_hotkey","target":{"sound":5},"hotkey":"alt+KeyA"}"#).unwrap();
        assert!(matches!(c, Command::SetHotkey { target: HotkeyTarget::Sound(5), .. }));
        let c: Command = serde_json::from_str(r#"{"cmd":"set_hotkey","target":"stop","hotkey":null}"#).unwrap();
        assert!(matches!(c, Command::SetHotkey { target: HotkeyTarget::Stop, hotkey: None }));
        let c: Command = serde_json::from_str(r#"{"cmd":"set_volume","output":"mic","value":0.5}"#).unwrap();
        assert!(matches!(c, Command::SetVolume { output: Output::Mic, .. }));
        let c: Command = serde_json::from_str(r#"{"cmd":"set_speaker_device","id":null}"#).unwrap();
        assert!(matches!(c, Command::SetSpeakerDevice { id: None }));
    }

    #[test]
    fn missing_state_serializes() {
        let j = serde_json::to_value(LoadState::Missing).unwrap();
        assert_eq!(j["status"], "missing");
    }

    #[test]
    fn mic_boost_command_parses_and_is_off_by_default() {
        let c: Command = serde_json::from_str(r#"{"cmd":"set_mic_boost","on":true}"#).unwrap();
        assert!(matches!(c, Command::SetMicBoost { on: true }));
        assert!(!Settings::default().mic_boost);
    }

    #[test]
    fn load_state_serializes_flat() {
        let v = SoundView {
            id: 1,
            name: "a".into(),
            path: "/a".into(),
            hotkey: None,
            hotkey_label: None,
            added: Some(1_700_000_000),
            state: LoadState::Ready { duration: 1.5 },
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["status"], "ready");
        assert_eq!(j["duration"], 1.5);
        assert_eq!(j["added"], 1_700_000_000);
    }

    // ---------- headless core: the real command handling, no devices or hotkeys ----------

    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh, empty folder for one test.
    fn tmpdir(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "soundpad-core-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            name
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A short silent mono WAV file.
    fn wav(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let (rate, frames) = (8000u32, 800u32);
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(frames * 2).to_le_bytes());
        b.resize(b.len() + frames as usize * 2, 0);
        std::fs::write(&path, b).unwrap();
        path
    }

    fn core_in(dir: &Path) -> Core {
        Core::headless(dir.join("soundpad.json"))
    }

    fn add(core: &mut Core, paths: &[&PathBuf]) {
        core.command(Command::AddFiles { paths: paths.iter().map(|p| (*p).clone()).collect(), playlist: None })
            .unwrap();
    }

    /// Lets the background decoder finish.
    fn wait_loaded(core: &mut Core) {
        for _ in 0..500 {
            core.tick();
            if core.sounds.iter().all(|s| !matches!(s.state, LoadState::Loading)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("sounds didn't finish loading");
    }

    fn names(core: &Core) -> Vec<&str> {
        core.sounds.iter().map(|s| s.name.as_str()).collect()
    }

    fn ids(core: &Core) -> Vec<u64> {
        core.sounds.iter().map(|s| s.id).collect()
    }

    fn playlist(core: &Core, id: u64) -> &PlaylistEntry {
        core.playlists.iter().find(|p| p.id == id).unwrap()
    }

    #[test]
    fn adding_files_folders_and_unsupported_files() {
        let dir = tmpdir("add");
        let a = wav(&dir, "Air_horn.wav");
        let folder = dir.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        wav(&folder, "b.wav");
        wav(&folder, "a.wav");
        std::fs::write(folder.join("notes.txt"), "not audio").unwrap();
        let text = dir.join("readme.txt");
        std::fs::write(&text, "x").unwrap();

        let mut core = core_in(&dir);
        let before = library::now_secs();
        add(&mut core, &[&a, &folder, &text]);
        // Underscores become spaces; folders are added sorted; unsupported files skipped.
        assert_eq!(names(&core), ["Air horn", "a", "b"]);
        assert!(core.state().messages.iter().any(|m| m == "Skipped 1 unsupported file(s)"));
        assert!(core.sounds.iter().all(|s| s.added.is_some_and(|t| t >= before)), "date added is recorded");

        // Adding the same file again doesn't duplicate it.
        add(&mut core, &[&a]);
        assert_eq!(core.sounds.len(), 3);

        wait_loaded(&mut core);
        assert!(core.sounds.iter().all(|s| matches!(s.state, LoadState::Ready { .. })));
    }

    #[test]
    fn adding_files_straight_into_a_playlist() {
        let dir = tmpdir("add-to-playlist");
        let mut core = core_in(&dir);
        core.command(Command::CreatePlaylist { name: "Memes".into(), ids: vec![] }).unwrap();
        let pl = core.playlists[0].id;
        let f = wav(&dir, "x.wav");
        core.command(Command::AddFiles { paths: vec![f], playlist: Some(pl) }).unwrap();
        assert_eq!(playlist(&core, pl).sounds, ids(&core));
    }

    #[test]
    fn playlists_create_rename_edit_and_delete() {
        let dir = tmpdir("playlists");
        let (a, b, c) = (wav(&dir, "a.wav"), wav(&dir, "b.wav"), wav(&dir, "c.wav"));
        let mut core = core_in(&dir);
        add(&mut core, &[&a, &b, &c]);
        let [ia, ib, ic] = ids(&core)[..] else { unreachable!() };

        // Names are made unique; unknown and repeated ids are ignored; the new list becomes active.
        core.command(Command::CreatePlaylist { name: "  New Playlist ".into(), ids: vec![ib, 999, ib, ia] }).unwrap();
        core.command(Command::CreatePlaylist { name: "New Playlist".into(), ids: vec![] }).unwrap();
        let (p1, p2) = (core.playlists[0].id, core.playlists[1].id);
        assert_eq!(playlist(&core, p1).name, "New Playlist");
        assert_eq!(playlist(&core, p2).name, "New Playlist 2");
        assert_eq!(playlist(&core, p1).sounds, [ib, ia]);
        assert_eq!(core.active_playlist, Some(p2));

        assert_eq!(
            core.command(Command::RenamePlaylist { id: p1, name: "   ".into() }),
            Err("Name can't be empty".into())
        );
        core.command(Command::RenamePlaylist { id: p1, name: " Memes ".into() }).unwrap();
        assert_eq!(playlist(&core, p1).name, "Memes");

        // Adding keeps one copy of each sound, appended in order.
        core.command(Command::AddToPlaylist { playlist: p1, ids: vec![ic, ia] }).unwrap();
        assert_eq!(playlist(&core, p1).sounds, [ib, ia, ic]);
        core.command(Command::MoveInPlaylist { playlist: p1, id: ic, to_index: 0 }).unwrap();
        assert_eq!(playlist(&core, p1).sounds, [ic, ib, ia]);
        core.command(Command::MoveInPlaylist { playlist: p1, id: ic, to_index: 99 }).unwrap();
        assert_eq!(playlist(&core, p1).sounds, [ib, ia, ic], "index past the end moves to the end");
        assert!(core.command(Command::MoveInPlaylist { playlist: p2, id: ia, to_index: 0 }).is_err());
        core.command(Command::RemoveFromPlaylist { playlist: p1, ids: vec![ib] }).unwrap();
        assert_eq!(playlist(&core, p1).sounds, [ia, ic]);
        assert_eq!(core.sounds.len(), 3, "removing from a playlist keeps the sound in the library");

        core.command(Command::SetActivePlaylist { id: Some(p1) }).unwrap();
        assert!(core.command(Command::SetActivePlaylist { id: Some(12345) }).is_err());
        assert_eq!(core.active_playlist, Some(p1));
        core.command(Command::DeletePlaylist { id: p1 }).unwrap();
        assert_eq!(core.active_playlist, None, "deleting the active list shows All Sounds");
        assert_eq!(core.playlists.len(), 1);
    }

    #[test]
    fn removing_renaming_and_reordering_sounds() {
        let dir = tmpdir("library");
        let (a, b, c) = (wav(&dir, "a.wav"), wav(&dir, "b.wav"), wav(&dir, "c.wav"));
        let mut core = core_in(&dir);
        add(&mut core, &[&a, &b, &c]);
        let [ia, ib, ic] = ids(&core)[..] else { unreachable!() };
        core.command(Command::CreatePlaylist { name: "P".into(), ids: vec![ia, ib] }).unwrap();

        core.command(Command::Rename { id: ia, name: "  Airhorn ".into() }).unwrap();
        assert_eq!(core.sounds[0].name, "Airhorn");
        assert!(core.command(Command::Rename { id: ia, name: "".into() }).is_err());
        assert!(core.command(Command::Rename { id: 999, name: "x".into() }).is_err());

        core.command(Command::Move { id: ic, to_index: 0 }).unwrap();
        assert_eq!(ids(&core), [ic, ia, ib]);
        core.command(Command::Move { id: ic, to_index: 50 }).unwrap();
        assert_eq!(ids(&core), [ia, ib, ic]);

        core.command(Command::Remove { id: ia }).unwrap();
        assert_eq!(ids(&core), [ib, ic]);
        assert_eq!(core.playlists[0].sounds, [ib], "removed sounds leave every playlist too");
    }

    #[test]
    fn one_hotkey_does_one_thing() {
        let dir = tmpdir("hotkeys");
        let (a, b) = (wav(&dir, "a.wav"), wav(&dir, "b.wav"));
        let mut core = core_in(&dir);
        add(&mut core, &[&a, &b]);
        let [ia, ib] = ids(&core)[..] else { unreachable!() };
        let set =
            |core: &mut Core, target, key: &str| core.command(Command::SetHotkey { target, hotkey: Some(key.into()) });

        set(&mut core, HotkeyTarget::Sound(ia), "alt+Digit1").unwrap();
        assert_eq!(core.state().sounds[0].hotkey_label.as_deref(), Some("⌥1"));
        // Giving the same combination to another sound, or to Stop, takes it away.
        set(&mut core, HotkeyTarget::Sound(ib), "alt+Digit1").unwrap();
        assert_eq!(core.sounds[0].hotkey, None);
        assert!(core.sounds[1].hotkey.is_some());
        set(&mut core, HotkeyTarget::Stop, "alt+Digit1").unwrap();
        assert_eq!(core.sounds[1].hotkey, None);
        assert_eq!(core.state().stop_hotkey_label.as_deref(), Some("⌥1"));

        // Bare letters would swallow typing everywhere.
        assert!(set(&mut core, HotkeyTarget::Pause, "KeyA").is_err());
        set(&mut core, HotkeyTarget::Pause, "F5").unwrap();
        core.command(Command::SetHotkey { target: HotkeyTarget::Pause, hotkey: None }).unwrap();
        assert_eq!(core.settings.pause_hotkey, None);
    }

    #[test]
    fn volume_limits_and_the_150_percent_option() {
        let mut core = core_in(&tmpdir("volume"));
        let vol = |core: &mut Core, output, value| core.command(Command::SetVolume { output, value }).unwrap();

        vol(&mut core, Output::Speakers, 1.4);
        assert_eq!(core.settings.speaker_volume, 1.0, "speakers stop at 100%");
        vol(&mut core, Output::Speakers, -1.0);
        assert_eq!(core.settings.speaker_volume, 0.0);

        assert!(!core.settings.mic_boost, "boost is off by default");
        vol(&mut core, Output::Mic, 1.4);
        assert_eq!(core.settings.mic_volume, 1.0, "mic stops at 100% without the boost");
        core.command(Command::SetMicBoost { on: true }).unwrap();
        vol(&mut core, Output::Mic, 1.4);
        assert_eq!(core.settings.mic_volume, 1.4);
        vol(&mut core, Output::Mic, 3.0);
        assert_eq!(core.settings.mic_volume, 1.5, "boost stops at 150%");
        core.command(Command::SetMicBoost { on: false }).unwrap();
        assert_eq!(core.settings.mic_volume, 1.0, "turning the boost off brings it back to 100%");

        core.command(Command::SetPassthroughVolume { value: 5.0 }).unwrap();
        assert_eq!(core.settings.passthrough_volume, 2.0);
        core.command(Command::SetOutputEnabled { output: Output::Mic, on: false }).unwrap();
        assert!(!core.settings.mic_enabled);
    }

    #[test]
    fn the_library_survives_a_restart() {
        let dir = tmpdir("restart");
        let (a, b) = (wav(&dir, "a.wav"), wav(&dir, "b.wav"));
        let added = {
            let mut core = core_in(&dir);
            add(&mut core, &[&a, &b]);
            let [ia, ib] = ids(&core)[..] else { unreachable!() };
            core.command(Command::Rename { id: ia, name: "Airhorn".into() }).unwrap();
            core.command(Command::SetHotkey { target: HotkeyTarget::Sound(ia), hotkey: Some("alt+KeyH".into()) })
                .unwrap();
            core.command(Command::CreatePlaylist { name: "Memes".into(), ids: vec![ib, ia] }).unwrap();
            core.command(Command::SetMicBoost { on: true }).unwrap();
            core.command(Command::SetVolume { output: Output::Mic, value: 1.25 }).unwrap();
            core.command(Command::SetPlaybackMode { mode: PlaybackMode::Loop }).unwrap();
            core.save();
            core.sounds[0].added
        };

        let mut core = core_in(&dir);
        assert_eq!(names(&core), ["Airhorn", "b"]);
        assert_eq!(core.sounds[0].hotkey, Some(hotkeys::validate_capture("alt+KeyH").unwrap()));
        assert_eq!(core.sounds[0].added, added, "date added is kept");
        assert_eq!(core.playlists.len(), 1);
        assert_eq!(core.playlists[0].name, "Memes");
        let [na, nb] = ids(&core)[..] else { unreachable!() };
        assert_eq!(core.playlists[0].sounds, [nb, na], "playlist order is kept");
        assert!(core.settings.mic_boost);
        assert_eq!(core.settings.mic_volume, 1.25);
        assert_eq!(core.settings.playback_mode, PlaybackMode::Loop);
        wait_loaded(&mut core);
        assert!(core.command(Command::Play { id: na }).is_ok());
    }

    #[test]
    fn a_damaged_library_file_is_kept_not_overwritten() {
        let dir = tmpdir("damaged");
        let config = dir.join("soundpad.json");
        std::fs::write(&config, "{ this is not json").unwrap();
        let mut core = Core::headless(config.clone());
        assert!(core.sounds.is_empty());
        core.save();
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("damaged"))
            .collect();
        assert_eq!(backups.len(), 1, "a copy of the damaged file is kept");
        assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), "{ this is not json");
    }

    #[test]
    fn missing_files_at_startup_while_running_and_restored() {
        let dir = tmpdir("missing");
        let (a, b) = (wav(&dir, "a.wav"), wav(&dir, "b.wav"));
        {
            let mut core = core_in(&dir);
            add(&mut core, &[&a, &b]);
            core.save();
        }

        // Deleted before launch: marked missing at startup, with one notice.
        std::fs::remove_file(&a).unwrap();
        let mut core = core_in(&dir);
        assert!(matches!(core.sounds[0].state, LoadState::Missing));
        assert!(core.state().messages.iter().any(|m| m == "1 sound file(s) are missing"));
        let ia = core.sounds[0].id;
        assert_eq!(core.command(Command::Play { id: ia }), Err("“a” is missing. Was it moved or deleted?".into()));
        wait_loaded(&mut core);

        // Deleted while running: caught before playing (hotkeys rely on the periodic check).
        let ib = core.sounds[1].id;
        std::fs::remove_file(&b).unwrap();
        assert!(core.command(Command::Play { id: ib }).is_err());
        assert!(matches!(core.sounds[1].state, LoadState::Missing));
        assert!(!core.player.has_sound(ib), "the player forgets it, so hotkeys can't play it");

        // Put back: the periodic check reloads it.
        wav(&dir, "b.wav");
        assert!(core.check_missing_files());
        wait_loaded(&mut core);
        assert!(matches!(core.sounds[1].state, LoadState::Ready { .. }));
        assert!(core.command(Command::Play { id: ib }).is_ok());

        // Retry Loading on a still-missing file keeps it missing.
        core.command(Command::Retry { id: ia }).unwrap();
        assert!(matches!(core.sounds[0].state, LoadState::Missing));
    }

    #[test]
    fn playing_needs_a_loaded_sound() {
        let dir = tmpdir("play");
        let a = wav(&dir, "a.wav");
        let mut core = core_in(&dir);
        add(&mut core, &[&a]);
        let id = core.sounds[0].id;
        // Right after adding it may still be decoding.
        if matches!(core.sounds[0].state, LoadState::Loading) {
            assert_eq!(core.command(Command::Play { id }), Err("Still loading…".into()));
        }
        wait_loaded(&mut core);
        assert!(core.command(Command::Play { id }).is_ok());
        assert!(core.command(Command::Play { id: 999 }).is_err());

        let bad = dir.join("broken.wav");
        std::fs::write(&bad, "RIFF nonsense").unwrap();
        add(&mut core, &[&bad]);
        wait_loaded(&mut core);
        let bid = core.sounds[1].id;
        assert!(matches!(core.sounds[1].state, LoadState::Failed { .. }));
        assert!(core.command(Command::Play { id: bid }).unwrap_err().starts_with("Can't play “broken”"));
    }

    #[test]
    fn messages_are_shown_once() {
        let dir = tmpdir("messages");
        let text = dir.join("x.txt");
        std::fs::write(&text, "x").unwrap();
        let mut core = core_in(&dir);
        add(&mut core, &[&text]);
        assert_eq!(core.state().messages.len(), 1);
        assert!(core.state().messages.is_empty(), "state() hands each message to the UI once");
    }
}

//! Persistent sound list + settings, and background decoding.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};

use crate::audio::decode::{SoundRef, decode_file};
use crate::audio::player::PlaybackMode;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedSound {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub hotkey: Option<String>,
    /// When the sound was added, in seconds since 1970. `None` for sounds
    /// added before this was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
}

/// Seconds since 1970, for `SavedSound::added`.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Speaker/headphone device id; `None` = system default.
    pub speaker_device: Option<String>,
    /// Virtual microphone (loopback) output; `None` = off.
    pub mic_device: Option<String>,
    /// Real microphone mixed into the virtual mic; `None` = off, `Some("")` = system default.
    pub input_device: Option<String>,
    /// The user has made an explicit choice for `mic_device` (disables auto-detect).
    pub mic_device_chosen: bool,
    pub speakers_enabled: bool,
    pub mic_enabled: bool,
    pub speaker_volume: f32,
    pub mic_volume: f32,
    pub passthrough_volume: f32,
    /// Let the virtual-mic volume go up to 150% (off by default; the UI warns).
    pub mic_boost: bool,
    /// Allow several sounds at once (otherwise a new sound replaces the current one).
    pub overlap: bool,
    pub stop_hotkey: Option<String>,
    pub pause_hotkey: Option<String>,
    pub playback_mode: PlaybackMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            speaker_device: None,
            mic_device: None,
            input_device: None,
            mic_device_chosen: false,
            speakers_enabled: true,
            mic_enabled: true,
            speaker_volume: 0.8,
            mic_volume: 0.8,
            passthrough_volume: 1.0,
            mic_boost: false,
            overlap: false,
            stop_hotkey: None,
            pause_hotkey: None,
            playback_mode: PlaybackMode::Once,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SavedState {
    pub sounds: Vec<SavedSound>,
    pub settings: Settings,
    pub playlists: Vec<SavedPlaylist>,
}

/// A named, ordered selection of library sounds (referenced by file path).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SavedPlaylist {
    pub name: String,
    pub sounds: Vec<PathBuf>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Soundpad")
}

/// `$SOUNDPAD_CONFIG` overrides the location (handy for testing).
pub fn config_path() -> PathBuf {
    std::env::var_os("SOUNDPAD_CONFIG").map(PathBuf::from).unwrap_or_else(|| config_dir().join("soundpad.json"))
}

pub fn load_from(path: &Path) -> SavedState {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| eprintln!("bad config {path:?}: {e}")).ok())
        .unwrap_or_default()
}

/// Atomic write (temp file + rename) so a crash never corrupts the library.
pub fn save_to(path: &Path, state: &SavedState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(tmp, path)
}

/// Pretty name from a file path ("Air Horn" from ".../air_horn.mp3").
pub fn display_name(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("Sound").replace('_', " ")
}

pub type LoadResult = (u64, Result<SoundRef, String>);

/// Thread pool decoding files in the background.
pub struct Loader {
    jobs: Sender<(u64, PathBuf)>,
    pub results: Receiver<LoadResult>,
}

impl Loader {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (jobs, job_rx) = unbounded::<(u64, PathBuf)>();
        let (res_tx, results) = unbounded::<LoadResult>();
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
        for i in 0..n {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            let wake = wake.clone();
            std::thread::Builder::new()
                .name(format!("decoder-{i}"))
                .spawn(move || {
                    for (id, path) in job_rx {
                        let r = decode_file(&path).map(Arc::new);
                        if res_tx.send((id, r)).is_err() {
                            break;
                        }
                        wake();
                    }
                })
                .expect("spawn decoder thread");
        }
        Self { jobs, results }
    }

    pub fn load(&self, id: u64, path: PathBuf) {
        let _ = self.jobs.send((id, path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_forward_compat() {
        let dir = std::env::temp_dir().join(format!("soundpad-test-{}", std::process::id()));
        let path = dir.join("cfg.json");
        let mut st = SavedState::default();
        st.sounds.push(SavedSound { path: "/a/b.mp3".into(), name: "b".into(), hotkey: Some("alt+KeyB".into()), added: Some(1_700_000_000) });
        st.settings.overlap = true;
        save_to(&path, &st).unwrap();
        assert_eq!(load_from(&path), st);
        // Missing fields fall back to defaults; unknown fields are ignored.
        std::fs::write(&path, r#"{"sounds":[{"path":"/x.wav","name":"x"}],"future":1}"#).unwrap();
        let st2 = load_from(&path);
        assert_eq!(st2.sounds[0].hotkey, None);
        assert_eq!(st2.sounds[0].added, None);
        assert_eq!(st2.settings, Settings::default());
        // Corrupt file -> defaults, not a crash.
        std::fs::write(&path, "{nope").unwrap();
        assert_eq!(load_from(&path), SavedState::default());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name(Path::new("/x/air_horn.mp3")), "air horn");
    }
}

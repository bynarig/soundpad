//! System-wide hotkeys (Carbon `RegisterEventHotKey` under the hood — no
//! Accessibility permission required). Hotkey events trigger the `Player`
//! directly from the event handler, independent of the UI.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::RwLock;

use crate::audio::player::Player;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Play(u64),
    Stop,
    TogglePause,
}

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    actions: Arc<RwLock<HashMap<u32, Action>>>,
    registered: HashMap<u32, HotKey>,
    pub init_error: Option<String>,
}

impl Hotkeys {
    /// Must be called on the main thread.
    pub fn new(player: Arc<Player>) -> Self {
        let actions: Arc<RwLock<HashMap<u32, Action>>> = Arc::default();
        let (manager, init_error) = match GlobalHotKeyManager::new() {
            Ok(m) => (Some(m), None),
            Err(e) => (None, Some(format!("Global hotkeys unavailable: {e}"))),
        };
        let map = actions.clone();
        let log = std::env::var_os("SOUNDPAD_LOG").is_some();
        GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
            if ev.state() != HotKeyState::Pressed {
                return;
            }
            let t = std::time::Instant::now();
            let action = map.read().get(&ev.id()).copied();
            let ok = match action {
                Some(Action::Play(id)) => player.play(id),
                Some(Action::Stop) => {
                    player.stop_all();
                    true
                }
                Some(Action::TogglePause) => {
                    player.toggle_pause();
                    true
                }
                None => false,
            };
            if log {
                eprintln!("[hotkey] {action:?} handled={ok} in {:.0} µs", t.elapsed().as_secs_f64() * 1e6);
            }
        }));
        Self { manager, actions, registered: HashMap::new(), init_error }
    }

    /// No system-wide hotkeys at all (headless tests: registering real ones
    /// would grab the developer's key combinations).
    pub fn disabled() -> Self {
        Self { manager: None, actions: Arc::default(), registered: HashMap::new(), init_error: None }
    }

    /// Make the registered set equal to `wanted`. Returns per-hotkey errors.
    pub fn sync(&mut self, wanted: &[(String, Action)]) -> Vec<(String, String)> {
        let mut errors = Vec::new();
        let Some(manager) = self.manager.as_ref() else { return errors };

        let mut want: HashMap<u32, (HotKey, Action)> = HashMap::new();
        for (s, a) in wanted {
            match HotKey::from_str(s) {
                Ok(hk) => {
                    want.insert(hk.id(), (hk, *a));
                }
                Err(e) => errors.push((s.clone(), e.to_string())),
            }
        }

        let stale: Vec<u32> = self.registered.keys().filter(|id| !want.contains_key(id)).copied().collect();
        for id in stale {
            if let Some(hk) = self.registered.remove(&id) {
                let _ = manager.unregister(hk);
            }
        }
        let mut actions = HashMap::new();
        for (id, (hk, a)) in want {
            if let std::collections::hash_map::Entry::Vacant(e) = self.registered.entry(id) {
                if let Err(e) = manager.register(hk) {
                    errors.push((hk.into_string(), format!("{e} (already used by another app?)")));
                    continue;
                }
                e.insert(hk);
            }
            actions.insert(id, a);
        }
        *self.actions.write() = actions;
        errors
    }
}

/// Canonical string for storage, e.g. `"shift+alt+KeyA"`.
pub fn canonical(s: &str) -> Option<String> {
    HotKey::from_str(s).ok().map(|h| h.into_string())
}

/// Human-friendly label using macOS symbols, e.g. `⌥⇧A`.
pub fn pretty(s: &str) -> String {
    let Ok(hk) = HotKey::from_str(s) else { return s.to_string() };
    let mut out = String::new();
    if hk.mods.contains(Modifiers::CONTROL) {
        out.push('⌃');
    }
    if hk.mods.contains(Modifiers::ALT) {
        out.push('⌥');
    }
    if hk.mods.contains(Modifiers::SHIFT) {
        out.push('⇧');
    }
    if hk.mods.contains(Modifiers::META) {
        out.push('⌘');
    }
    let key = hk.key.to_string();
    let key = key.strip_prefix("Key").or_else(|| key.strip_prefix("Digit")).map(str::to_string).unwrap_or_else(|| {
        match hk.key {
            Code::Numpad0
            | Code::Numpad1
            | Code::Numpad2
            | Code::Numpad3
            | Code::Numpad4
            | Code::Numpad5
            | Code::Numpad6
            | Code::Numpad7
            | Code::Numpad8
            | Code::Numpad9 => key.replace("Numpad", "Num "),
            Code::Space => "Space".into(),
            Code::ArrowUp => "↑".into(),
            Code::ArrowDown => "↓".into(),
            Code::ArrowLeft => "←".into(),
            Code::ArrowRight => "→".into(),
            Code::Enter => "↩".into(),
            Code::Escape => "Esc".into(),
            _ => key.clone(),
        }
    });
    out.push_str(&key);
    out
}

/// Validate a combination recorded by the UI (e.g. `"shift+alt+KeyA"`) and
/// return its canonical form. Bare keys are only allowed for function keys:
/// a global hotkey on a plain letter would swallow that letter everywhere.
pub fn validate_capture(s: &str) -> Result<String, String> {
    let hk = HotKey::from_str(s).map_err(|e| format!("Unsupported key: {e}"))?;
    let key = hk.key.to_string();
    let is_function_key = key.len() >= 2 && key.starts_with('F') && key[1..].chars().all(|c| c.is_ascii_digit());
    if hk.mods.is_empty() && !is_function_key {
        return Err("Add a modifier (⌃ ⌥ ⇧ ⌘) — a bare key would block typing everywhere".into());
    }
    Ok(hk.into_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_and_display() {
        let s = validate_capture("alt+shift+KeyA").unwrap();
        assert_eq!(canonical(&s).as_deref(), Some(s.as_str()));
        assert_eq!(pretty(&s), "⌥⇧A");
        assert_eq!(pretty(&validate_capture("meta+Digit1").unwrap()), "⌘1");
        assert_eq!(pretty(&validate_capture("control+shift+KeyF").unwrap()), "⌃⇧F");
        // bare function keys are fine; bare letters are rejected
        assert!(validate_capture("F5").is_ok());
        assert!(validate_capture("KeyA").is_err());
        assert!(validate_capture("alt+NotAKey").is_err());
    }
}

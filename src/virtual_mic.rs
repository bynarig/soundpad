//! The built-in "Soundpad Virtual Mic" driver (a renamed BlackHole build, see
//! `vendor/blackhole-driver`). It is shipped inside `Soundpad.app` and copied
//! to `/Library/Audio/Plug-Ins/HAL` on request, which needs admin rights.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::audio::engine::DeviceInfo;

/// The record-only device other apps (Discord, Zoom…) use as their microphone.
pub const DEVICE_NAME: &str = "Soundpad Virtual Mic";
/// The play-only device Soundpad plays into; it feeds `DEVICE_NAME`. Older
/// drivers had a single two-way device, and playing into a device that also has
/// a microphone side makes macOS ask for microphone permission.
pub const FEED_NAME: &str = "Soundpad Virtual Mic Feed";
pub const DRIVER_BUNDLE: &str = "SoundpadVirtualMic.driver";
pub const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";

pub fn installed_path() -> PathBuf {
    Path::new(HAL_DIR).join(DRIVER_BUNDLE)
}

pub fn is_installed() -> bool {
    installed_path().join("Contents/Info.plist").exists()
}

/// The driver shipped with this copy of the app.
pub fn bundled_driver() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Soundpad.app/Contents/MacOS/Soundpad -> Soundpad.app/Contents/Resources/...
    let in_bundle = exe.parent()?.parent()?.join("Resources").join(DRIVER_BUNDLE);
    if in_bundle.exists() {
        return Some(in_bundle);
    }
    // `cargo run`: use the one built by scripts/build_driver.sh.
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/driver").join(DRIVER_BUNDLE);
    dev.exists().then_some(dev)
}

fn bundle_version(driver: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(driver.join("Contents/Info.plist")).ok()?;
    let after = plist.split("<key>CFBundleVersion</key>").nth(1)?;
    let v = after.split("<string>").nth(1)?.split("</string>").next()?;
    Some(v.trim().to_string())
}

/// True when the installed driver is older/different than the bundled one.
pub fn update_available() -> bool {
    match (bundled_driver().and_then(|b| bundle_version(&b)), bundle_version(&installed_path())) {
        (Some(new), Some(old)) => new != old,
        _ => false,
    }
}

/// The output Soundpad plays into: the Feed device, or the two-way device of
/// an older driver until it is updated.
pub fn find_device(outputs: &[DeviceInfo]) -> Option<&DeviceInfo> {
    outputs.iter().find(|d| d.name == FEED_NAME).or_else(|| outputs.iter().find(|d| d.name == DEVICE_NAME))
}

/// Either side of Soundpad's own virtual mic.
pub fn is_virtual_mic(d: &DeviceInfo) -> bool {
    d.name == DEVICE_NAME || d.name == FEED_NAME
}

/// Name of the device to record from to hear what was played into `output`.
pub fn recording_side(output: &str) -> &str {
    if output == FEED_NAME { DEVICE_NAME } else { output }
}

/// The virtual-mic output to use while the user hasn't picked one: Soundpad
/// Virtual Mic if installed, otherwise none. Other loopback devices (BlackHole,
/// Loopback…) are never taken automatically: other apps such as OBS often use
/// them, and Soundpad's sounds would leak into those recordings.
pub fn auto_mic_device(outputs: &[DeviceInfo]) -> Option<String> {
    find_device(outputs).map(|d| d.id.clone())
}

/// Another loopback device (e.g. BlackHole) the user could choose instead of ours.
pub fn other_loopback(outputs: &[DeviceInfo]) -> Option<&DeviceInfo> {
    outputs.iter().find(|d| d.looks_virtual() && !is_virtual_mic(d))
}

/// Run a shell command as root via the standard macOS password dialog.
fn run_as_admin(shell: &str, prompt: &str, args: &[&str]) -> Result<(), String> {
    // Arguments are passed through argv and quoted by AppleScript itself.
    let script = format!(
        "on run argv\n do shell script \"{shell}\" with prompt \"{prompt}\" with administrator privileges\nend run"
    );
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run osascript: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("-128") {
        Err("Cancelled".into())
    } else {
        Err(err.trim().to_string())
    }
}

/// Restart CoreAudio so it loads/unloads plug-ins. Audio stops for ~1 s.
const RESTART_AUDIO: &str =
    "launchctl kickstart -k system/com.apple.audio.coreaudiod 2>/dev/null || killall coreaudiod";

/// Install (or update) the driver. Blocks until the user answers the password prompt.
pub fn install() -> Result<(), String> {
    let src = bundled_driver().ok_or("The virtual mic driver is missing from this build of Soundpad.")?;
    let src = src.to_str().ok_or("unsupported path")?;
    let dest = installed_path();
    let dest = dest.to_str().unwrap();
    // `item 1 of argv` is the source path, inserted with AppleScript's quoting.
    let shell = format!(
        "set -e; mkdir -p {HAL_DIR}; rm -rf '{dest}'; cp -R \" & quoted form of item 1 of argv & \" {HAL_DIR}/; \
         chown -R root:wheel '{dest}'; {RESTART_AUDIO}"
    );
    run_as_admin(&shell, "Soundpad wants to install its virtual microphone.", &[src])
}

pub fn uninstall() -> Result<(), String> {
    let dest = installed_path();
    let shell = format!("rm -rf '{}'; {RESTART_AUDIO}", dest.display());
    run_as_admin(&shell, "Soundpad wants to remove its virtual microphone.", &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo { id: id.into(), name: name.into() }
    }

    #[test]
    fn blackhole_is_never_picked_automatically() {
        let speakers = dev("spk", "MacBook Pro Speakers");
        let blackhole = dev("bh", "BlackHole 2ch");
        let ours = dev("sp", DEVICE_NAME);
        // Only BlackHole installed: nothing is taken, but it is offered.
        let outs = [speakers.clone(), blackhole.clone()];
        assert_eq!(auto_mic_device(&outs), None);
        assert_eq!(other_loopback(&outs).map(|d| d.name.as_str()), Some("BlackHole 2ch"));
        // Both installed: ours wins regardless of order.
        let outs = [blackhole.clone(), speakers.clone(), ours.clone()];
        assert_eq!(auto_mic_device(&outs).as_deref(), Some("sp"));
        assert_eq!(other_loopback(&outs).map(|d| d.id.as_str()), Some("bh"));
        // New driver: Soundpad plays into the Feed, never the two-way device.
        let outs = [dev("old", DEVICE_NAME), dev("feed", FEED_NAME)];
        assert_eq!(auto_mic_device(&outs).as_deref(), Some("feed"));
        assert_eq!(auto_mic_device(&outs[..1]).as_deref(), Some("old"), "older driver still works");
        assert!(other_loopback(&outs).is_none(), "the Feed isn't offered as a foreign loopback");
        assert_eq!(recording_side(FEED_NAME), DEVICE_NAME);
        assert_eq!(recording_side("BlackHole 2ch"), "BlackHole 2ch");
        // Neither: nothing.
        assert_eq!(auto_mic_device(std::slice::from_ref(&speakers)), None);
        assert!(other_loopback(&[speakers, ours]).is_none());
    }

    #[test]
    fn system_default_mic_skips_loopback_devices() {
        use crate::audio::engine::pick_default_input;
        let inputs = [dev("bh", "BlackHole 2ch"), dev("sp", DEVICE_NAME), dev("mic", "MacBook Pro Microphone")];
        // Default input is BlackHole (common with OBS setups): use the real mic instead.
        assert_eq!(pick_default_input(Some("bh"), &inputs).as_deref(), Some("mic"));
        assert_eq!(pick_default_input(Some("sp"), &inputs).as_deref(), Some("mic"));
        assert_eq!(pick_default_input(Some("mic"), &inputs).as_deref(), Some("mic"));
        assert_eq!(pick_default_input(None, &inputs).as_deref(), Some("mic"));
        // Only loopbacks: no microphone rather than a feedback loop.
        assert_eq!(pick_default_input(Some("bh"), &inputs[..2]), None);
    }

    #[test]
    fn reads_bundle_version() {
        let d = std::env::temp_dir().join(format!("sp-drv-{}", std::process::id()));
        std::fs::create_dir_all(d.join("Contents")).unwrap();
        std::fs::write(
            d.join("Contents/Info.plist"),
            "<dict>\n<key>CFBundleVersion</key><string> 0.7.1 </string>\n</dict>",
        )
        .unwrap();
        assert_eq!(bundle_version(&d).as_deref(), Some("0.7.1"));
        std::fs::remove_dir_all(d).ok();
    }
}

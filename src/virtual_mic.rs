//! The built-in "Soundpad Virtual Mic" driver (a renamed BlackHole build, see
//! `vendor/blackhole-driver`). It is shipped inside `Soundpad.app` and copied
//! to `/Library/Audio/Plug-Ins/HAL` on request, which needs admin rights.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::audio::engine::DeviceInfo;

pub const DEVICE_NAME: &str = "Soundpad Virtual Mic";
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

pub fn find_device(devices: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devices.iter().find(|d| is_virtual_mic(d))
}

pub fn is_virtual_mic(d: &DeviceInfo) -> bool {
    d.name == DEVICE_NAME
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

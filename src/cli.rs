//! Command-line modes shared by the Rust binary and the macOS app executable.

/// Returns an exit code, or -1 when no CLI flag was given.
pub fn run() -> i32 {
    let has = |f: &str| std::env::args().any(|a| a == f);
    if has("--selftest") {
        return crate::selftest::run();
    }
    for (flag, install) in [("--install-virtual-mic", true), ("--uninstall-virtual-mic", false)] {
        if has(flag) {
            let r = if install { crate::virtual_mic::install() } else { crate::virtual_mic::uninstall() };
            return match r {
                Ok(()) => {
                    println!("{} Soundpad Virtual Mic", if install { "Installed" } else { "Removed" });
                    0
                }
                Err(e) => {
                    eprintln!("Failed: {e}");
                    1
                }
            };
        }
    }
    if has("--help") || has("-h") {
        println!(
            "Soundpad\n  --selftest [--with-mic]   test audio on this Mac's real devices (silent)\n  \
             --install-virtual-mic      install the Soundpad Virtual Mic driver\n  \
             --uninstall-virtual-mic    remove it"
        );
        return 0;
    }
    -1
}

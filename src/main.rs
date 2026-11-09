//! Developer entry point: CLI modes only. The app itself is the SwiftUI
//! front end in `macos/` (built by `scripts/bundle.sh`).

fn main() {
    match soundpad::cli::run() {
        -1 => {
            eprintln!("The Soundpad UI is the native macOS app: run scripts/bundle.sh and open dist/Soundpad.app.");
            eprintln!("CLI: soundpad --selftest [--with-mic] | --install-virtual-mic | --uninstall-virtual-mic");
            std::process::exit(2);
        }
        code => std::process::exit(code),
    }
}

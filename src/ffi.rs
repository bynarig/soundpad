//! C ABI for the native macOS front end (see `macos/Bridge/soundpad.h`).
//!
//! Data crosses the boundary as JSON (state snapshots and commands) except for
//! the per-frame playback query, which uses a plain struct. Strings returned
//! by Rust must be released with `sp_string_free`. Every entry point catches
//! panics so they never unwind into Swift.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::{Command, Core};

pub struct SPCore(Core);

#[repr(C)]
pub struct SPPlayback {
    pub sound_id: u64,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub voices: u32,
    pub peak: f32,
}

pub type SPWakeFn = extern "C" fn(ctx: *mut c_void);

struct WakeCtx(*mut c_void);
// SAFETY: the Swift side guarantees the context outlives the core and that the
// callback may be invoked from any thread (it only dispatches to main).
unsafe impl Send for WakeCtx {}
unsafe impl Sync for WakeCtx {}

fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        eprintln!("soundpad: internal error (panic) caught at FFI boundary");
        default
    })
}

fn to_c(s: String) -> *mut c_char {
    CString::new(s.replace('\0', "")).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

/// Create the core. Must be called on the main thread.
/// `config_path` may be NULL for the default location (or `$SOUNDPAD_CONFIG`).
///
/// # Safety
/// `config_path` must be NULL or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_new(config_path: *const c_char, wake: SPWakeFn, ctx: *mut c_void) -> *mut SPCore {
    guard(std::ptr::null_mut(), || {
        let path = if config_path.is_null() {
            crate::library::config_path()
        } else {
            PathBuf::from(unsafe { CStr::from_ptr(config_path) }.to_string_lossy().into_owned())
        };
        let ctx = Arc::new(WakeCtx(ctx));
        let waker: crate::core::Waker = Arc::new(move || wake(ctx.0));
        Box::into_raw(Box::new(SPCore(Core::new(path, waker))))
    })
}

/// # Safety
/// `core` must come from `sp_core_new` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_free(core: *mut SPCore) {
    if !core.is_null() {
        guard((), || drop(unsafe { Box::from_raw(core) }));
    }
}

/// Housekeeping; returns true when the UI should re-read the state.
///
/// # Safety
/// `core` must be a live pointer from `sp_core_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_tick(core: *mut SPCore) -> bool {
    let core = unsafe { &mut *core };
    guard(false, || core.0.tick())
}

/// JSON snapshot of everything the UI shows. Free with `sp_string_free`.
///
/// # Safety
/// `core` must be a live pointer from `sp_core_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_state(core: *mut SPCore) -> *mut c_char {
    let core = unsafe { &mut *core };
    guard(std::ptr::null_mut(), || to_c(serde_json::to_string(&core.0.state()).unwrap_or_default()))
}

/// Execute a JSON command. Returns NULL on success, or an error message
/// (free with `sp_string_free`).
///
/// # Safety
/// `core` must be live; `json` a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_command(core: *mut SPCore, json: *const c_char) -> *mut c_char {
    let core = unsafe { &mut *core };
    let json = unsafe { CStr::from_ptr(json) }.to_string_lossy().into_owned();
    guard(to_c("internal error".into()), || {
        let result = serde_json::from_str::<Command>(&json)
            .map_err(|e| format!("bad command {json}: {e}"))
            .and_then(|cmd| core.0.command(cmd));
        match result {
            Ok(()) => std::ptr::null_mut(),
            Err(e) => to_c(e),
        }
    })
}

/// Cheap playback query for animating the UI.
///
/// # Safety
/// `core` must be live; `out` must point to writable memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_core_playback(core: *mut SPCore, out: *mut SPPlayback) {
    let core = unsafe { &*core };
    guard((), || {
        let p = core.0.playback();
        unsafe {
            *out = SPPlayback {
                sound_id: p.sound_id,
                position: p.position_secs,
                duration: p.duration_secs,
                paused: p.paused,
                voices: p.active_voices,
                peak: p.peak,
            };
        }
    })
}

/// # Safety
/// `s` must be NULL or a string returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sp_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Handle command-line modes (`--selftest`, `--install-virtual-mic`, …).
/// Returns the exit code, or -1 when no CLI flag was given (start the GUI).
#[unsafe(no_mangle)]
pub extern "C" fn sp_cli_main() -> i32 {
    guard(1, crate::cli::run)
}

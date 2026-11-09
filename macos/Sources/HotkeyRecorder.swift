import AppKit
import Carbon.HIToolbox
import SwiftUI

/// Records a key combination. Global hotkeys are released while it is open
/// so the press reaches us instead of triggering a sound.
struct HotkeyRecorder: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let target: HotkeyTarget

    @State private var liveModifiers: NSEvent.ModifierFlags = []
    @State private var error: String?
    @State private var monitor: Any?
    @Namespace private var ns

    var body: some View {
        VStack(spacing: 18) {
            VStack(spacing: 4) {
                Text("Set Hotkey").font(.title3.bold())
                Text("Press a key combination for \(store.targetName(target)).")
                    .foregroundStyle(.secondary)
            }

            GlassEffectContainer {
                Text(display)
                    .font(.system(size: 30, weight: .semibold, design: .rounded))
                    .contentTransition(.numericText())
                    .frame(minWidth: 160, minHeight: 56)
                    .padding(.horizontal, 22)
                    .glassEffect(.regular.tint(.accentColor.opacity(liveModifiers.isEmpty ? 0 : 0.25)), in: .capsule)
                    .glassEffectID("cap", in: ns)
            }
            .animation(.spring(duration: 0.25), value: display)

            if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .multilineTextAlignment(.center)
            } else {
                Text("Works system-wide, even when Soundpad is in the background.\nEsc cancels · Delete clears")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }

            HStack {
                Button("Clear", role: .destructive) { finish(nil) }
                    .disabled(store.hotkeyLabel(target) == nil)
                Spacer()
                Button("Cancel") { dismiss() }
            }
        }
        .padding(24)
        .frame(width: 380)
        .onAppear(perform: start)
        .onDisappear(perform: stop)
    }

    private var display: String {
        if !liveModifiers.isEmpty { return symbols(liveModifiers) + "…" }
        return store.hotkeyLabel(target) ?? "—"
    }

    private func start() {
        store.send("begin_capture")
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { event in
            handle(event) ? nil : event
        }
    }

    private func stop() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        store.send("end_capture")
    }

    /// Returns true when the event was consumed.
    private func handle(_ e: NSEvent) -> Bool {
        let mods = e.modifierFlags.intersection([.control, .option, .shift, .command])
        if e.type == .flagsChanged {
            liveModifiers = mods
            return true
        }
        if mods.isEmpty, e.keyCode == UInt16(kVK_Escape) {
            dismiss()
            return true
        }
        if mods.isEmpty, e.keyCode == UInt16(kVK_Delete) || e.keyCode == UInt16(kVK_ForwardDelete) {
            finish(nil)
            return true
        }
        guard let token = KeyCodes.token(for: e.keyCode) else {
            error = String(localized: "That key can't be used as a hotkey")
            return true
        }
        var s = ""
        if mods.contains(.shift) { s += "shift+" }
        if mods.contains(.control) { s += "control+" }
        if mods.contains(.option) { s += "alt+" }
        if mods.contains(.command) { s += "meta+" }
        s += token
        finish(s)
        return true
    }

    private func finish(_ hotkey: String?) {
        let err = store.trySend("set_hotkey", ["target": target.json, "hotkey": hotkey ?? NSNull()])
        if let err {
            error = localizeCoreMessage(err)
            liveModifiers = []
        } else {
            dismiss()
        }
    }
}

func symbols(_ m: NSEvent.ModifierFlags) -> String {
    var s = ""
    if m.contains(.control) { s += "⌃" }
    if m.contains(.option) { s += "⌥" }
    if m.contains(.shift) { s += "⇧" }
    if m.contains(.command) { s += "⌘" }
    return s
}

/// macOS virtual key codes → key names understood by the Rust hotkey layer.
enum KeyCodes {
    static func token(for code: UInt16) -> String? { map[Int(code)] }

    private static let map: [Int: String] = {
        var m: [Int: String] = [
            kVK_ANSI_A: "KeyA", kVK_ANSI_B: "KeyB", kVK_ANSI_C: "KeyC", kVK_ANSI_D: "KeyD", kVK_ANSI_E: "KeyE",
            kVK_ANSI_F: "KeyF", kVK_ANSI_G: "KeyG", kVK_ANSI_H: "KeyH", kVK_ANSI_I: "KeyI", kVK_ANSI_J: "KeyJ",
            kVK_ANSI_K: "KeyK", kVK_ANSI_L: "KeyL", kVK_ANSI_M: "KeyM", kVK_ANSI_N: "KeyN", kVK_ANSI_O: "KeyO",
            kVK_ANSI_P: "KeyP", kVK_ANSI_Q: "KeyQ", kVK_ANSI_R: "KeyR", kVK_ANSI_S: "KeyS", kVK_ANSI_T: "KeyT",
            kVK_ANSI_U: "KeyU", kVK_ANSI_V: "KeyV", kVK_ANSI_W: "KeyW", kVK_ANSI_X: "KeyX", kVK_ANSI_Y: "KeyY",
            kVK_ANSI_Z: "KeyZ",
            kVK_ANSI_0: "Digit0", kVK_ANSI_1: "Digit1", kVK_ANSI_2: "Digit2", kVK_ANSI_3: "Digit3",
            kVK_ANSI_4: "Digit4", kVK_ANSI_5: "Digit5", kVK_ANSI_6: "Digit6", kVK_ANSI_7: "Digit7",
            kVK_ANSI_8: "Digit8", kVK_ANSI_9: "Digit9",
            kVK_ANSI_Keypad0: "Numpad0", kVK_ANSI_Keypad1: "Numpad1", kVK_ANSI_Keypad2: "Numpad2",
            kVK_ANSI_Keypad3: "Numpad3", kVK_ANSI_Keypad4: "Numpad4", kVK_ANSI_Keypad5: "Numpad5",
            kVK_ANSI_Keypad6: "Numpad6", kVK_ANSI_Keypad7: "Numpad7", kVK_ANSI_Keypad8: "Numpad8",
            kVK_ANSI_Keypad9: "Numpad9", kVK_ANSI_KeypadPlus: "NumpadAdd", kVK_ANSI_KeypadMinus: "NumpadSubtract",
            kVK_ANSI_KeypadMultiply: "NumpadMultiply", kVK_ANSI_KeypadDivide: "NumpadDivide",
            kVK_ANSI_KeypadDecimal: "NumpadDecimal", kVK_ANSI_KeypadEnter: "NumpadEnter",
            kVK_ANSI_KeypadEquals: "NumpadEqual",
            kVK_Space: "Space", kVK_Return: "Enter", kVK_Tab: "Tab",
            kVK_UpArrow: "ArrowUp", kVK_DownArrow: "ArrowDown", kVK_LeftArrow: "ArrowLeft",
            kVK_RightArrow: "ArrowRight", kVK_Home: "Home", kVK_End: "End", kVK_PageUp: "PageUp",
            kVK_PageDown: "PageDown",
            kVK_ANSI_Minus: "Minus", kVK_ANSI_Equal: "Equal", kVK_ANSI_LeftBracket: "BracketLeft",
            kVK_ANSI_RightBracket: "BracketRight", kVK_ANSI_Semicolon: "Semicolon", kVK_ANSI_Quote: "Quote",
            kVK_ANSI_Comma: "Comma", kVK_ANSI_Period: "Period", kVK_ANSI_Slash: "Slash",
            kVK_ANSI_Backslash: "Backslash", kVK_ANSI_Grave: "Backquote",
            kVK_Escape: "Escape",
        ]
        let fkeys = [
            kVK_F1, kVK_F2, kVK_F3, kVK_F4, kVK_F5, kVK_F6, kVK_F7, kVK_F8, kVK_F9, kVK_F10,
            kVK_F11, kVK_F12, kVK_F13, kVK_F14, kVK_F15, kVK_F16, kVK_F17, kVK_F18, kVK_F19, kVK_F20,
        ]
        for (i, k) in fkeys.enumerated() { m[k] = "F\(i + 1)" }
        return m
    }()
}

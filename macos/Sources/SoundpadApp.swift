import AppKit
import SwiftUI

@main
enum Entry {
    static func main() {
        // `Soundpad --selftest`, `--install-virtual-mic`, … run without UI.
        let code = sp_cli_main()
        if code >= 0 { exit(code) }
        // Single-window app: always open the main window instead of restoring a
        // previous session (which may have had it closed, leaving no window).
        UserDefaults.standard.set(false, forKey: "NSQuitAlwaysKeepsWindows")
        trace("entry: starting SwiftUI")
        SoundpadApp.main()
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    // Never restore a previous session: it may have had the main window
    // closed, which would leave a single-window app with no window at all.
    func application(_ app: NSApplication, shouldRestoreSecureApplicationState coder: NSCoder) -> Bool { false }
    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    func applicationDidFinishLaunching(_ notification: Notification) {
        MainActor.assumeIsolated { Store.shared.start() }
    }
}

struct SoundpadApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var store = Store.shared
    @FocusedValue(\.addSoundsAction) private var addSounds

    var body: some Scene {
        WindowGroup("Soundpad", id: "main") {
            ContentView()
                .environment(store)
        }
        .defaultSize(width: 680, height: 740)
        .defaultLaunchBehavior(.presented)
        .windowResizability(.contentMinSize)
        .commands {
            // Single-window app: replace "New Window" with "Add Sounds…".
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") {
                    Task { await Updater.shared.check(userInitiated: true) }
                    NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
                }
            }
            CommandGroup(replacing: .newItem) {
                Button("Add Sounds…") { addSounds?() }
                    .keyboardShortcut("o")
                    .disabled(addSounds == nil)
            }
            CommandMenu("Playback") {
                Button("Pause / Resume") { store.togglePause() }
                    .keyboardShortcut("p", modifiers: [.command, .shift])
                Button("Stop All") { store.stop() }
                    .keyboardShortcut(".")
                Divider()
                Button("Next") { store.next() }
                    .keyboardShortcut("]", modifiers: [.command, .option])
                Button("Previous") { store.previous() }
                    .keyboardShortcut("[", modifiers: [.command, .option])
                Button("Forward 10 Seconds") { store.seek(by: 10) }
                    .keyboardShortcut("]")
                Button("Back 10 Seconds") { store.seek(by: -10) }
                    .keyboardShortcut("[")
                Divider()
                Picker("When a Sound Ends", selection: Binding(
                    get: { store.state.settings.playbackMode }, set: { store.setMode($0) })
                ) {
                    ForEach(PlaybackMode.allCases) { Text($0.title).tag($0) }
                }
                Divider()
                Button("New Playlist") { store.createPlaylist() }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
            }
        }

        Settings {
            SettingsView()
                .environment(store)
        }
    }
}

func trace(_ s: String) {
    if ProcessInfo.processInfo.environment["SOUNDPAD_TRACE"] != nil {
        FileHandle.standardError.write(Data("[trace] \(s)\n".utf8))
    }
}

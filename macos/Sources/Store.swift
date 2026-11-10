import AppKit
import Foundation
import Observation

// MARK: - Models (decoded from the Rust core's JSON state)

struct Sound: Decodable, Identifiable, Hashable {
    let id: UInt64
    let name: String
    let path: String
    let hotkey: String?
    let hotkeyLabel: String?
    let status: String
    let duration: Double?
    let error: String?
    /// Seconds since 1970; nil for sounds added before dates were recorded.
    let added: Double?

    var addedDate: Date? { added.map { Date(timeIntervalSince1970: $0) } }

    var isReady: Bool { status == "ready" }
    var isLoading: Bool { status == "loading" }
    var isFailed: Bool { status == "failed" }
    /// The file was moved or deleted.
    var isMissing: Bool { status == "missing" }
    /// Tooltip: the file's path, or why it can't play.
    var tooltip: String {
        if isMissing { return String(localized: "File not found: \(path)") }
        return error ?? path
    }
}

struct CoreSettings: Decodable, Equatable {
    var speakerDevice: String?
    var micDevice: String?
    var inputDevice: String?
    var speakersEnabled: Bool
    var micEnabled: Bool
    var speakerVolume: Double
    var micVolume: Double
    var passthroughVolume: Double
    var micBoost: Bool
    var overlap: Bool
    var stopHotkey: String?
    var pauseHotkey: String?
    var playbackMode: PlaybackMode
}

/// What happens when a sound ends.
enum PlaybackMode: String, Decodable, CaseIterable, Identifiable {
    case once, loop, playlist
    var id: String { rawValue }

    var title: String {
        switch self {
        case .once: String(localized: "Play Once")
        case .loop: String(localized: "Repeat Sound")
        case .playlist: String(localized: "Play List")
        }
    }

    var detail: String {
        switch self {
        case .once: String(localized: "Stop when the sound ends")
        case .loop: String(localized: "Repeat the current sound")
        case .playlist: String(localized: "Continue with the next sound, then start over")
        }
    }

    var icon: String {
        switch self {
        case .once: "1.circle"
        case .loop: "repeat.1"
        case .playlist: "repeat"
        }
    }
}

struct Playlist: Decodable, Identifiable, Hashable {
    let id: UInt64
    let name: String
    let sounds: [UInt64]
}

struct Device: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let isVirtual: Bool
    let isSoundpad: Bool
}

struct DriverInfo: Decodable, Equatable {
    let bundled: Bool
    let installed: Bool
    let updateAvailable: Bool
    let busy: Bool
    let selected: Bool
}

struct Diagnostic: Decodable, Hashable {
    let output: String
    let device: String
    let sampleRate: Int
    let channels: Int
    let bufferFrames: Int
    let dspLoadPercent: Double
    let dspPeakPercent: Double
    let micUnderruns: Int
}

struct AppState: Decodable {
    var sounds: [Sound] = []
    var playlists: [Playlist] = []
    var activePlaylist: UInt64?
    var settings = CoreSettings(
        speakerDevice: nil, micDevice: nil, inputDevice: nil, speakersEnabled: true, micEnabled: true,
        speakerVolume: 0.8, micVolume: 0.8, passthroughVolume: 1, micBoost: false, overlap: false, stopHotkey: nil, pauseHotkey: nil,
        playbackMode: .once)
    var stopHotkeyLabel: String?
    var pauseHotkeyLabel: String?
    var outputs: [Device] = []
    var inputs: [Device] = []
    var defaultInputName: String?
    var otherLoopback: Device?
    var speakerDeviceName: String?
    var speakerIsVirtual = false
    var driver = DriverInfo(bundled: false, installed: false, updateAvailable: false, busy: false, selected: false)
    var diagnostics: [Diagnostic] = []
    var inputDeviceName: String?
    var configPath: String = ""
    var messages: [String] = []
}

enum HotkeyTarget: Identifiable, Hashable {
    case sound(UInt64)
    case stop
    case pause

    var id: String {
        switch self {
        case .sound(let id): "sound-\(id)"
        case .stop: "stop"
        case .pause: "pause"
        }
    }

    var json: Any {
        switch self {
        case .sound(let id): ["sound": id]
        case .stop: "stop"
        case .pause: "pause"
        }
    }
}

enum OutputKind: String { case speakers, mic }

struct Toast: Identifiable, Equatable {
    let id = UUID()
    let text: String
}

/// Fast-changing playback info, kept separate so only the views that show
/// progress re-render. Values are quantized to what is visible on screen
/// (whole seconds, ~1/400 of the bar, 5 meter segments), so SwiftUI only
/// redraws when something visibly changes instead of every frame.
@Observable @MainActor
final class PlaybackModel {
    var soundID: UInt64 = 0
    /// Elapsed whole seconds (for the time label).
    var elapsed: Int = 0
    var duration: Double = 0
    var paused = false
    var voices: UInt32 = 0
    /// Progress 0...1 in steps of 1/400.
    var fraction: Double = 0
    /// Output level 0...1 (-60…0 dBFS), with meter ballistics: instant rise,
    /// smooth fall. Quantized so views redraw only on visible changes.
    var level: Double = 0
    /// Peak-hold mark 0...1: holds the recent maximum, then falls.
    var peakHold: Double = 0

    var isPlaying: Bool { soundID != 0 }

    static let progressSteps = 400.0
    static let meterRange = 60.0        // dB shown, -60…0 dBFS
    static let meterFallPerTick = 0.03  // ~0.9 of the scale per second at 30 Hz
    static let peakHoldTicks = 30       // ~1 s
}

// MARK: - Store: owns the Rust core

@Observable @MainActor
final class Store {
    static let shared = Store()

    private(set) var state = AppState()
    let playback = PlaybackModel()
    private(set) var toasts: [Toast] = []

    @ObservationIgnored private var core: OpaquePointer?
    @ObservationIgnored private var tickTimer: Timer?
    @ObservationIgnored private var frameTimer: Timer?
    @ObservationIgnored private var wakePending = false
    @ObservationIgnored private let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    private init() {}

    /// Start the Rust core. Must run after NSApplication has finished
    /// launching: the core installs Carbon hotkey handlers on the application
    /// event target, which breaks SwiftUI's start-up if done earlier.
    func start() {
        guard core == nil else { return }
        trace("store start")
        core = sp_core_new(nil, { _ in
            // Any thread → coalesce onto the main thread.
            DispatchQueue.main.async { MainActor.assumeIsolated { Store.shared.wake() } }
        }, nil)
        refresh()
        Updater.shared.checkOnLaunch()
        tickTimer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { _ in
            MainActor.assumeIsolated { Store.shared.tick() }
        }
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated { Store.shared.shutdown() }
        }
    }

    private func wake() {
        guard !wakePending else { return }
        wakePending = true
        DispatchQueue.main.async { [self] in
            wakePending = false
            refresh()
            updatePlayback()
        }
    }

    private func tick() {
        guard let core else { return }
        if sp_core_tick(core) { refresh() }
        updatePlayback()
    }

    func shutdown() {
        tickTimer?.invalidate()
        frameTimer?.invalidate()
        if let core { sp_core_free(core) }  // saves the library
        core = nil
    }

    func refresh() {
        trace("refresh")
        guard let core, let raw = sp_core_state(core) else { return }
        defer { sp_string_free(raw) }
        let data = Data(bytes: raw, count: strlen(raw))
        do {
            var s = try decoder.decode(AppState.self, from: data)
            let msgs = s.messages
            s.messages = []
            state = s
            msgs.map(localizeCoreMessage).forEach(show)
        } catch {
            show("Internal error: \(error)")
        }
    }

    @ObservationIgnored private var peakHoldLeft = 0

    /// Poll playback; runs at display rate only while something plays.
    private func updatePlayback() {
        guard let core else { return }
        var p = SPPlayback()
        sp_core_playback(core, &p)
        let pb = playback
        let elapsed = Int(max(0, p.position).rounded())
        let fraction = p.duration > 0
            ? (min(max(p.position / p.duration, 0), 1) * PlaybackModel.progressSteps).rounded() / PlaybackModel.progressSteps
            : 0
        let db = 20 * log10(max(Double(p.peak), 1e-6))
        let target = min(max((db + PlaybackModel.meterRange) / PlaybackModel.meterRange, 0), 1)
        let level = ((target >= pb.level ? target : max(target, pb.level - PlaybackModel.meterFallPerTick)) * 200).rounded() / 200
        var hold = pb.peakHold
        if level >= hold {
            hold = level
            peakHoldLeft = PlaybackModel.peakHoldTicks
        } else if peakHoldLeft > 0 {
            peakHoldLeft -= 1
        } else {
            hold = max(level, hold - PlaybackModel.meterFallPerTick)
        }
        if pb.soundID != p.sound_id { pb.soundID = p.sound_id }
        if pb.elapsed != elapsed { pb.elapsed = elapsed }
        if pb.duration != p.duration { pb.duration = p.duration }
        if pb.paused != p.paused { pb.paused = p.paused }
        if pb.voices != p.voices { pb.voices = p.voices }
        if pb.fraction != fraction { pb.fraction = fraction }
        if pb.level != level { pb.level = level }
        if pb.peakHold != hold { pb.peakHold = hold }

        let animating = p.sound_id != 0 && !p.paused
        if animating, frameTimer == nil {
            // 30 Hz is plenty for a meter; views only redraw on visible changes.
            let t = Timer(timeInterval: 1.0 / 30.0, repeats: true) { _ in
                MainActor.assumeIsolated { Store.shared.updatePlayback() }
            }
            RunLoop.main.add(t, forMode: .common)
            frameTimer = t
        } else if !animating, let t = frameTimer {
            t.invalidate()
            frameTimer = nil
            if pb.level != 0 { pb.level = 0 }
            if pb.peakHold != 0 { pb.peakHold = 0 }
        }
    }

    // MARK: Commands

    @discardableResult
    func send(_ cmd: String, _ args: [String: Any] = [:]) -> Bool {
        guard let core else { return false }
        var obj = args
        obj["cmd"] = cmd
        guard let data = try? JSONSerialization.data(withJSONObject: obj),
              let json = String(data: data, encoding: .utf8)
        else { return false }
        let err = json.withCString { sp_core_command(core, $0) }
        refresh()
        updatePlayback()
        if let err {
            show(localizeCoreMessage(String(cString: err)))
            sp_string_free(err)
            return false
        }
        return true
    }

    /// Like `send` but returns the error instead of showing it.
    func trySend(_ cmd: String, _ args: [String: Any] = [:]) -> String? {
        guard let core else { return "not running" }
        var obj = args
        obj["cmd"] = cmd
        guard let data = try? JSONSerialization.data(withJSONObject: obj),
              let json = String(data: data, encoding: .utf8)
        else { return "encoding error" }
        let err = json.withCString { sp_core_command(core, $0) }
        refresh()
        guard let err else { return nil }
        defer { sp_string_free(err) }
        return String(cString: err)
    }

    func show(_ text: String) {
        let t = Toast(text: text)
        toasts.append(t)
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [self] in
            toasts.removeAll { $0.id == t.id }
        }
    }

    // Convenience wrappers
    func play(_ id: UInt64) { send("play", ["id": id]) }
    func next() { send("next") }
    func previous() { send("previous") }
    func seek(to secs: Double) { send("seek", ["secs": secs]) }
    func seek(by secs: Double) { send("seek_by", ["secs": secs]) }
    func setMode(_ m: PlaybackMode) { send("set_playback_mode", ["mode": m.rawValue]) }

    // Playlists
    var activePlaylist: Playlist? { state.activePlaylist.flatMap { id in state.playlists.first { $0.id == id } } }
    func playlist(_ id: UInt64) -> Playlist? { state.playlists.first { $0.id == id } }
    /// Sounds shown in the main list: the active playlist, or the whole library.
    var visibleSounds: [Sound] {
        guard let pl = activePlaylist else { return state.sounds }
        let byID = Dictionary(uniqueKeysWithValues: state.sounds.map { ($0.id, $0) })
        return pl.sounds.compactMap { byID[$0] }
    }
    func showPlaylist(_ id: UInt64?) {
        if state.activePlaylist != id { send("set_active_playlist", ["id": id.map { $0 as Any } ?? NSNull()]) }
    }
    func createPlaylist(with ids: some Sequence<UInt64> = []) {
        send("create_playlist", ["name": String(localized: "New Playlist"), "ids": Array(ids)])
    }
    func renamePlaylist(_ id: UInt64, _ name: String) { send("rename_playlist", ["id": id, "name": name]) }
    func deletePlaylist(_ id: UInt64) { send("delete_playlist", ["id": id]) }
    func addToPlaylist(_ playlist: UInt64, _ ids: some Sequence<UInt64>) {
        send("add_to_playlist", ["playlist": playlist, "ids": Array(ids)])
    }
    func removeFromPlaylist(_ playlist: UInt64, _ ids: some Sequence<UInt64>) {
        send("remove_from_playlist", ["playlist": playlist, "ids": Array(ids)])
    }
    func moveInPlaylist(_ playlist: UInt64, _ id: UInt64, to index: Int) {
        send("move_in_playlist", ["playlist": playlist, "id": id, "to_index": index])
    }
    /// Add files to the library, and to `playlist` when given.
    func add(_ urls: [URL], to playlist: UInt64?) {
        var args: [String: Any] = ["paths": urls.map(\.path)]
        if let playlist { args["playlist"] = playlist }
        send("add_files", args)
    }
    func stop() { send("stop") }
    func togglePause() { send("toggle_pause") }
    func remove(_ ids: some Sequence<UInt64>) { ids.forEach { send("remove", ["id": $0]) } }
    func add(_ urls: [URL]) { send("add_files", ["paths": urls.map(\.path)]) }
    func rename(_ id: UInt64, _ name: String) { send("rename", ["id": id, "name": name]) }
    func move(_ id: UInt64, to index: Int) { send("move", ["id": id, "to_index": index]) }
    func reveal(_ id: UInt64) { send("reveal", ["id": id]) }
    func retry(_ id: UInt64) { send("retry", ["id": id]) }
    func clearHotkey(_ t: HotkeyTarget) { send("set_hotkey", ["target": t.json, "hotkey": NSNull()]) }
    func setEnabled(_ o: OutputKind, _ on: Bool) { send("set_output_enabled", ["output": o.rawValue, "on": on]) }
    func setVolume(_ o: OutputKind, _ v: Double) { send("set_volume", ["output": o.rawValue, "value": v]) }

    func sound(_ id: UInt64) -> Sound? { state.sounds.first { $0.id == id } }

    func hotkeyLabel(_ t: HotkeyTarget) -> String? {
        switch t {
        case .sound(let id): sound(id)?.hotkeyLabel
        case .stop: state.stopHotkeyLabel
        case .pause: state.pauseHotkeyLabel
        }
    }

    func targetName(_ t: HotkeyTarget) -> String {
        switch t {
        case .sound(let id): sound(id).map { "“\($0.name)”" } ?? String(localized: "this sound")
        case .stop: String(localized: "Stop")
        case .pause: String(localized: "Pause / Resume")
        }
    }
}

func formatTime(_ s: Double) -> String {
    let t = Int(max(0, s).rounded())
    if t >= 3600 { return String(format: "%d:%02d:%02d", t / 3600, t / 60 % 60, t % 60) }
    return String(format: "%d:%02d", t / 60, t % 60)
}

func formatLength(_ s: Double) -> String {
    s < 60 ? String(format: "%.1fs", s) : formatTime(s)
}

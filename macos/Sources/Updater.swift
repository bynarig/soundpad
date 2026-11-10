import AppKit
import CryptoKit
import Foundation
import Observation

/// Self-contained auto-updater.
///
/// The app reads a small JSON feed (`appcast.json`, see `scripts/release.sh`):
///
///     { "version": "0.2.0", "url": "https://…/Soundpad-0.2.0.zip",
///       "signature": "<base64 Ed25519 signature of the zip>",
///       "notes": "What's new", "minimumSystemVersion": "26.0" }
///
/// An update is installed only if the zip's Ed25519 signature matches the
/// public key built into this app (Info.plist `SoundpadUpdatePublicKey`), so
/// only whoever holds the private key can ship updates. Installing: download,
/// verify, unzip next to the app, check it is really Soundpad at that version,
/// then quit; a tiny script swaps the bundles and relaunches.
///
/// With automatic updates on, Soundpad checks at every launch (and daily while
/// running), downloads and verifies a new version in the background, and only
/// then offers "Restart to Update". Offline or failed background attempts are
/// skipped silently.
struct UpdateInfo: Decodable, Equatable {
    let version: String
    let url: URL
    let signature: String
    let notes: String?
    let minimumSystemVersion: String?
}

enum UpdatePhase: Equatable {
    case idle
    case checking
    case upToDate
    case available(UpdateInfo)
    case downloading(Double)
    /// Downloaded, verified and unpacked next to the app; waiting for the user.
    case ready(UpdateInfo, URL)
    case installing
    case failed(String)
}

@Observable @MainActor
final class Updater {
    static let shared = Updater()

    private(set) var phase: UpdatePhase = .idle

    @ObservationIgnored let bundle: Bundle
    @ObservationIgnored let feedURL: URL?
    @ObservationIgnored let publicKey: Curve25519.Signing.PublicKey?
    /// Quits the app once the install script is running (overridable for tests).
    @ObservationIgnored var terminate: () -> Void = { NSApp.terminate(nil) }

    var currentVersion: String {
        bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }

    var isConfigured: Bool { feedURL != nil && publicKey != nil }

    var automaticChecks: Bool {
        get { access(keyPath: \.automaticChecks); return UserDefaults.standard.object(forKey: Self.autoKey) as? Bool ?? true }
        set { withMutation(keyPath: \.automaticChecks) { UserDefaults.standard.set(newValue, forKey: Self.autoKey) } }
    }

    var availableUpdate: UpdateInfo? {
        if case .available(let info) = phase { return info }
        return nil
    }

    /// A downloaded update waiting for "Restart to Update".
    var readyUpdate: UpdateInfo? {
        if case .ready(let info, _) = phase { return info }
        return nil
    }

    private static let autoKey = "SoundpadAutoCheckUpdates"
    private static let lastCheckKey = "SoundpadLastUpdateCheck"
    @ObservationIgnored private var dailyTimer: Timer?

    init(bundle: Bundle = .main) {
        self.bundle = bundle
        let feed = bundle.object(forInfoDictionaryKey: "SoundpadUpdateFeedURL") as? String
        feedURL = feed.flatMap { $0.isEmpty ? nil : URL(string: $0) }
        let key = bundle.object(forInfoDictionaryKey: "SoundpadUpdatePublicKey") as? String
        publicKey = key.flatMap { Data(base64Encoded: $0) }.flatMap { try? Curve25519.Signing.PublicKey(rawRepresentation: $0) }
    }

    // MARK: Checking

    /// Quiet check at every launch, then daily while running. Downloads a new
    /// version in the background; nothing is shown if it fails (e.g. offline).
    func checkOnLaunch() {
        guard isConfigured else { return }
        Task { await automaticCheck() }
        dailyTimer?.invalidate()
        dailyTimer = Timer.scheduledTimer(withTimeInterval: 24 * 3600, repeats: true) { _ in
            Task { @MainActor in await Updater.shared.automaticCheck() }
        }
    }

    private func automaticCheck() async {
        guard automaticChecks else { return }
        if case .ready = phase { return }
        await check(userInitiated: false)
        if let info = availableUpdate { await prepareInBackground(info) }
    }

    /// Download, verify and unpack without bothering the user. On failure the
    /// update stays "available" (the Settings button still works) and the next
    /// launch tries again.
    private func prepareInBackground(_ info: UpdateInfo) async {
        do {
            let app = try await prepare(info)
            phase = .ready(info, app)
        } catch {
            phase = .available(info)
        }
    }

    func check(userInitiated: Bool) async {
        guard let feedURL, isConfigured else {
            if userInitiated { phase = .failed(String(localized: "Updates aren't set up for this build.")) }
            return
        }
        switch phase {
        case .checking, .downloading, .ready, .installing: return
        default: break
        }
        phase = .checking
        do {
            var req = URLRequest(url: feedURL, cachePolicy: .reloadIgnoringLocalAndRemoteCacheData, timeoutInterval: 20)
            req.setValue("Soundpad/\(currentVersion)", forHTTPHeaderField: "User-Agent")
            let (data, response) = try await URLSession.shared.data(for: req)
            if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                throw UpdateError.http(http.statusCode)
            }
            let info = try JSONDecoder().decode(UpdateInfo.self, from: data)
            UserDefaults.standard.set(Date(), forKey: Self.lastCheckKey)
            let newer = Self.compare(info.version, currentVersion) == .orderedDescending
            let supported = info.minimumSystemVersion.map(Self.systemSupports) ?? true
            phase = newer && supported ? .available(info) : .upToDate
        } catch {
            let msg = (error as? UpdateError)?.text ?? error.localizedDescription
            phase = userInitiated ? .failed(String(localized: "Couldn't check for updates: \(msg)")) : .idle
        }
    }

    // MARK: Installing

    /// Download, verify and install now (Settings button).
    func install(_ info: UpdateInfo) async {
        do {
            let newApp = try await prepare(info)
            phase = .installing
            try relaunch(into: newApp)
        } catch {
            let msg = (error as? UpdateError)?.text ?? error.localizedDescription
            phase = .failed(String(localized: "Couldn't install the update: \(msg)"))
        }
    }

    /// "Restart to Update": swap in the already downloaded version.
    func installReady() {
        guard case .ready(let info, let app) = phase else { return }
        guard FileManager.default.fileExists(atPath: app.path) else {
            // The staged copy was cleaned up; fetch it again.
            Task { await install(info) }
            return
        }
        phase = .installing
        do {
            try relaunch(into: app)
        } catch {
            phase = .failed(String(localized: "Couldn't install the update: \(error.localizedDescription)"))
        }
    }

    /// Downloads the update, checks its signature and unpacks it next to the app.
    private func prepare(_ info: UpdateInfo) async throws -> URL {
        guard let publicKey else { throw UpdateError.message(String(localized: "Updates aren't set up for this build.")) }
        let appURL = bundle.bundleURL
        let parent = appURL.deletingLastPathComponent()
        guard FileManager.default.isWritableFile(atPath: parent.path),
              FileManager.default.isWritableFile(atPath: appURL.path)
        else {
            throw UpdateError.message(String(localized: "Move Soundpad to your Applications folder to install updates."))
        }
        phase = .downloading(0)
        let data = try await download(info.url)
        guard let sig = Data(base64Encoded: info.signature), publicKey.isValidSignature(sig, for: data) else {
            throw UpdateError.message(String(localized: "The download isn't signed by the developer."))
        }
        return try await Task.detached { [bundleID = bundle.bundleIdentifier] in
            try Self.unpack(data, nextTo: appURL, version: info.version, bundleID: bundleID)
        }.value
    }

    private func relaunch(into newApp: URL) throws {
        try Self.launchSwapScript(old: bundle.bundleURL, new: newApp)
        terminate()
    }

    /// Downloads with a regular download task (reading byte by byte was very
    /// slow), reporting progress from the task.
    private func download(_ url: URL) async throws -> Data {
        var observation: NSKeyValueObservation?
        defer { observation?.invalidate() }
        let (file, response): (URL, URLResponse) = try await withCheckedThrowingContinuation { cont in
            let task = URLSession.shared.downloadTask(with: url) { location, response, error in
                guard let location, let response else {
                    cont.resume(throwing: error ?? URLError(.badServerResponse))
                    return
                }
                // The temporary file is deleted when this handler returns: keep it.
                let kept = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".zip")
                do {
                    try FileManager.default.moveItem(at: location, to: kept)
                    cont.resume(returning: (kept, response))
                } catch {
                    cont.resume(throwing: error)
                }
            }
            observation = task.progress.observe(\.fractionCompleted) { progress, _ in
                let f = progress.fractionCompleted
                Task { @MainActor in
                    if case .downloading = Updater.shared.phase { Updater.shared.phase = .downloading(f) }
                }
            }
            task.resume()
        }
        defer { try? FileManager.default.removeItem(at: file) }
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw UpdateError.http(http.statusCode)
        }
        return try Data(contentsOf: file)
    }

    /// Unzips into a staging folder on the same volume as the app and checks
    /// that it contains this app (same bundle id) at the advertised version.
    nonisolated private static func unpack(_ zip: Data, nextTo appURL: URL, version: String, bundleID: String?) throws -> URL {
        let fm = FileManager.default
        let staging = try fm.url(for: .itemReplacementDirectory, in: .userDomainMask, appropriateFor: appURL, create: true)
        let zipURL = staging.appendingPathComponent("update.zip")
        try zip.write(to: zipURL)
        let out = staging.appendingPathComponent("unpacked")
        try run("/usr/bin/ditto", ["-x", "-k", zipURL.path, out.path])
        let apps = try fm.contentsOfDirectory(at: out, includingPropertiesForKeys: nil).filter { $0.pathExtension == "app" }
        guard let app = apps.first, let b = Bundle(url: app),
              b.bundleIdentifier == bundleID,
              b.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String == version
        else { throw UpdateError.message(String(localized: "The downloaded update is damaged.")) }
        try run("/usr/bin/codesign", ["--verify", "--deep", app.path])
        return app
    }

    /// Waits for this process to exit, swaps the bundles (restoring the old one
    /// if anything fails) and relaunches.
    nonisolated private static func launchSwapScript(old: URL, new: URL) throws {
        let script = """
        pid="$1"; old="$2"; new="$3"; backup="$old.previous"
        while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
        rm -rf "$backup"
        if mv "$old" "$backup" && mv "$new" "$old"; then
          xattr -dr com.apple.quarantine "$old" 2>/dev/null
          rm -rf "$backup"
        else
          [ -d "$backup" ] && [ ! -d "$old" ] && mv "$backup" "$old"
        fi
        open "$old"
        """
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", script, "soundpad-update", String(getpid()), old.path, new.path]
        try p.run()
    }

    nonisolated private static func run(_ tool: String, _ args: [String]) throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            throw UpdateError.message(String(localized: "The downloaded update is damaged."))
        }
    }

    // MARK: Versions

    /// Numeric dotted-version comparison ("0.10.0" > "0.9.2").
    nonisolated static func compare(_ a: String, _ b: String) -> ComparisonResult {
        let pa = a.split(separator: ".").map { Int($0) ?? 0 }
        let pb = b.split(separator: ".").map { Int($0) ?? 0 }
        for i in 0..<max(pa.count, pb.count) {
            let x = i < pa.count ? pa[i] : 0
            let y = i < pb.count ? pb[i] : 0
            if x != y { return x < y ? .orderedAscending : .orderedDescending }
        }
        return .orderedSame
    }

    nonisolated private static func systemSupports(_ minimum: String) -> Bool {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        return compare("\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)", minimum) != .orderedAscending
    }
}

enum UpdateError: Error {
    case http(Int)
    case message(String)

    var text: String {
        switch self {
        case .http(let code): HTTPURLResponse.localizedString(forStatusCode: code) + " (\(code))"
        case .message(let m): m
        }
    }
}

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

    private static let autoKey = "SoundpadAutoCheckUpdates"
    private static let lastCheckKey = "SoundpadLastUpdateCheck"

    init(bundle: Bundle = .main) {
        self.bundle = bundle
        let feed = bundle.object(forInfoDictionaryKey: "SoundpadUpdateFeedURL") as? String
        feedURL = feed.flatMap { $0.isEmpty ? nil : URL(string: $0) }
        let key = bundle.object(forInfoDictionaryKey: "SoundpadUpdatePublicKey") as? String
        publicKey = key.flatMap { Data(base64Encoded: $0) }.flatMap { try? Curve25519.Signing.PublicKey(rawRepresentation: $0) }
    }

    // MARK: Checking

    /// Quiet daily check at launch (no errors shown).
    func checkOnLaunch() {
        guard isConfigured, automaticChecks else { return }
        let last = UserDefaults.standard.object(forKey: Self.lastCheckKey) as? Date ?? .distantPast
        guard Date().timeIntervalSince(last) > 24 * 3600 else { return }
        Task { await check(userInitiated: false) }
    }

    func check(userInitiated: Bool) async {
        guard let feedURL, isConfigured else {
            if userInitiated { phase = .failed(String(localized: "Updates aren't set up for this build.")) }
            return
        }
        switch phase {
        case .checking, .downloading, .installing: return
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

    func install(_ info: UpdateInfo) async {
        guard let publicKey else { return }
        let appURL = bundle.bundleURL
        let parent = appURL.deletingLastPathComponent()
        guard FileManager.default.isWritableFile(atPath: parent.path),
              FileManager.default.isWritableFile(atPath: appURL.path)
        else {
            phase = .failed(String(localized: "Move Soundpad to your Applications folder to install updates."))
            return
        }
        do {
            phase = .downloading(0)
            let data = try await download(info.url)
            guard let sig = Data(base64Encoded: info.signature), publicKey.isValidSignature(sig, for: data) else {
                throw UpdateError.message(String(localized: "The download isn't signed by the developer."))
            }
            phase = .installing
            let newApp = try await Task.detached { [bundleID = bundle.bundleIdentifier] in
                try Self.unpack(data, nextTo: appURL, version: info.version, bundleID: bundleID)
            }.value
            try Self.launchSwapScript(old: appURL, new: newApp)
            terminate()
        } catch {
            let msg = (error as? UpdateError)?.text ?? error.localizedDescription
            phase = .failed(String(localized: "Couldn't install the update: \(msg)"))
        }
    }

    private func download(_ url: URL) async throws -> Data {
        let (bytes, response) = try await URLSession.shared.bytes(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw UpdateError.http(http.statusCode)
        }
        let expected = max(response.expectedContentLength, 1)
        var data = Data()
        data.reserveCapacity(Int(max(response.expectedContentLength, 0)))
        var chunk = [UInt8]()
        chunk.reserveCapacity(1 << 16)
        for try await byte in bytes {
            chunk.append(byte)
            if chunk.count == 1 << 16 {
                data.append(contentsOf: chunk)
                chunk.removeAll(keepingCapacity: true)
                if response.expectedContentLength > 0 {
                    phase = .downloading(min(Double(data.count) / Double(expected), 1))
                }
            }
        }
        data.append(contentsOf: chunk)
        return data
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

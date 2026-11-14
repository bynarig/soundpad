import CryptoKit
import Foundation
import Observation

/// Downloads audio from YouTube, YouTube Music and SoundCloud links with
/// yt-dlp (https://github.com/yt-dlp/yt-dlp). Spotify streams are DRM-protected,
/// so for a Spotify song we read its public title and artist and download the
/// best match from YouTube instead.
///
/// Optional: nothing happens until it's turned on in Settings. Soundpad keeps
/// its own copy of yt-dlp (downloaded from the official GitHub release and
/// checked against the published SHA-256), because sites change often and old
/// versions stop working. A yt-dlp already on the Mac is the fallback.
@MainActor @Observable
final class MediaDownloader {
    static let shared = MediaDownloader()
    static let enabledKey = "linkDownloadsEnabled"

    // MARK: Sites

    enum Site: Equatable {
        case youtube, youtubeMusic, soundcloud, spotify

        var name: String {
            switch self {
            case .youtube: "YouTube"
            case .youtubeMusic: "YouTube Music"
            case .soundcloud: "SoundCloud"
            case .spotify: "Spotify"
            }
        }
    }

    /// The supported site a link points to, or nil.
    nonisolated static func site(for link: String) -> Site? {
        guard let url = URL(string: link.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = url.host()?.lowercased()
        else { return nil }
        func on(_ domain: String) -> Bool { host == domain || host.hasSuffix("." + domain) }
        if on("music.youtube.com") { return .youtubeMusic }
        if on("youtube.com") || on("youtu.be") || on("youtube-nocookie.com") { return .youtube }
        if on("soundcloud.com") || on("snd.sc") { return .soundcloud }
        if on("spotify.com") || on("spotify.link") { return .spotify }
        return nil
    }

    // MARK: yt-dlp

    enum Tool: Equatable {
        case missing
        case installing
        case ready(path: String, version: String, managed: Bool)
        case failed(String)
    }

    private(set) var tool: Tool = .missing

    /// Soundpad's own copy, kept up to date.
    nonisolated static var managedTool: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appending(path: "Soundpad/Tools/yt-dlp")
    }

    /// Where downloaded audio goes.
    nonisolated static var downloadsFolder: URL {
        FileManager.default.urls(for: .musicDirectory, in: .userDomainMask)[0]
            .appending(path: "Soundpad Downloads", directoryHint: .isDirectory)
    }

    var isEnabled: Bool { UserDefaults.standard.bool(forKey: Self.enabledKey) }

    /// Call at launch and when the feature is turned on: finds or installs
    /// yt-dlp, and updates Soundpad's copy at most once a day.
    func prepare() async {
        guard isEnabled, tool != .installing else { return }
        let managed = Self.managedTool.path
        if let version = await Self.version(of: managed) {
            tool = .ready(path: managed, version: version, managed: true)
            await updateIfDue()
            return
        }
        tool = .installing
        do {
            try await Self.install()
            if let version = await Self.version(of: managed) {
                tool = .ready(path: managed, version: version, managed: true)
                UserDefaults.standard.set(Date(), forKey: "ytDlpUpdated")
                return
            }
            throw DownloadError.message(String(localized: "The downloaded yt-dlp doesn't run."))
        } catch {
            // Offline or GitHub unreachable: use a yt-dlp that's already installed.
            if let system = await Self.systemTool(), let version = await Self.version(of: system) {
                tool = .ready(path: system, version: version, managed: false)
            } else {
                tool = .failed(error.localizedDescription)
            }
        }
    }

    /// Updates Soundpad's copy now ("Update" in Settings) or once a day.
    func updateIfDue(force: Bool = false) async {
        guard case .ready(let path, _, true) = tool else { return }
        let last = UserDefaults.standard.object(forKey: "ytDlpUpdated") as? Date ?? .distantPast
        guard force || Date().timeIntervalSince(last) > 86_400 else { return }
        tool = .installing
        _ = await Self.run(path, ["--update"])
        UserDefaults.standard.set(Date(), forKey: "ytDlpUpdated")
        if let version = await Self.version(of: path) {
            tool = .ready(path: path, version: version, managed: true)
        } else {
            tool = .missing
            await prepare()
        }
    }

    /// Downloads yt-dlp_macos from the latest GitHub release and checks it
    /// against the release's SHA2-256SUMS before installing it.
    nonisolated private static func install() async throws {
        let base = URL(string: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/")!
        let (sums, sumsResponse) = try await URLSession.shared.data(from: base.appending(path: "SHA2-256SUMS"))
        guard (sumsResponse as? HTTPURLResponse)?.statusCode == 200,
              let expected = String(decoding: sums, as: UTF8.self)
                .split(separator: "\n")
                .first(where: { $0.hasSuffix(" yt-dlp_macos") })?
                .split(separator: " ").first
        else { throw DownloadError.message(String(localized: "Couldn't download yt-dlp.")) }

        let (file, response) = try await URLSession.shared.download(from: base.appending(path: "yt-dlp_macos"))
        defer { try? FileManager.default.removeItem(at: file) }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            throw DownloadError.message(String(localized: "Couldn't download yt-dlp."))
        }
        let digest = SHA256.hash(data: try Data(contentsOf: file, options: .mappedIfSafe))
        guard digest.map({ String(format: "%02x", $0) }).joined() == expected.lowercased() else {
            throw DownloadError.message(String(localized: "The downloaded yt-dlp failed its checksum."))
        }
        let dest = managedTool
        try FileManager.default.createDirectory(at: dest.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? FileManager.default.removeItem(at: dest)
        try FileManager.default.moveItem(at: file, to: dest)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: dest.path)
    }

    /// A yt-dlp installed some other way (Homebrew, pip, …), searched on the
    /// login shell's PATH since apps start with a minimal one.
    nonisolated private static func systemTool() async -> String? {
        for path in ["/opt/homebrew/bin/yt-dlp", "/usr/local/bin/yt-dlp"]
        where FileManager.default.isExecutableFile(atPath: path) { return path }
        let found = await run("/bin/zsh", ["-lc", "command -v yt-dlp"]).out
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return FileManager.default.isExecutableFile(atPath: found) ? found : nil
    }

    nonisolated private static func version(of path: String) async -> String? {
        guard FileManager.default.isExecutableFile(atPath: path) else { return nil }
        let r = await run(path, ["--version"])
        let v = r.out.trimmingCharacters(in: .whitespacesAndNewlines)
        return r.status == 0 && !v.isEmpty ? v : nil
    }

    // MARK: Downloads

    struct Job: Identifiable, Equatable {
        enum State: Equatable { case running, done, failed(String), cancelled }
        let id = UUID()
        let link: String
        let site: Site
        var title: String
        var progress: Double?
        var state: State = .running
    }

    private(set) var jobs: [Job] = []
    @ObservationIgnored private var processes: [UUID: Process] = [:]

    var isBusy: Bool { jobs.contains { $0.state == .running } }

    /// Starts downloading `link`; the audio is added to the library and, when
    /// given, to `playlist`.
    func download(_ link: String, into playlist: UInt64?) {
        let link = link.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let site = Self.site(for: link) else { return }
        let job = Job(link: link, site: site, title: link)
        jobs.insert(job, at: 0)
        Task { await run(job.id, link: link, site: site, playlist: playlist) }
    }

    func cancel(_ id: UUID) {
        processes[id]?.terminate()
        update(id) { $0.state = .cancelled }
    }

    func clearFinished() { jobs.removeAll { $0.state != .running } }

    private func update(_ id: UUID, _ change: (inout Job) -> Void) {
        guard let i = jobs.firstIndex(where: { $0.id == id }) else { return }
        change(&jobs[i])
    }

    private func run(_ id: UUID, link: String, site: Site, playlist: UInt64?) async {
        guard case .ready(let exe, _, _) = tool else {
            update(id) { $0.state = .failed(String(localized: "yt-dlp isn't ready yet. Check Settings → Download from Links.")) }
            return
        }
        let work = FileManager.default.temporaryDirectory.appending(path: "soundpad-download-\(id.uuidString)")
        defer { try? FileManager.default.removeItem(at: work) }
        do {
            var target = link
            if site == .spotify {
                let song = try await Self.spotifySong(link)
                update(id) { $0.title = song.display }
                target = "ytsearch1:\(song.search)"
            }
            try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
            let result = await Self.run(exe, Self.arguments(for: target, into: work)) { [weak self] p in
                Task { @MainActor in self?.processes[id] = p }
            } onLine: { [weak self] line in
                let parsed = Self.parse(line)
                guard parsed.title != nil || parsed.progress != nil else { return }
                Task { @MainActor in
                    self?.update(id) { job in
                        if let t = parsed.title { job.title = t }
                        if let p = parsed.progress { job.progress = p }
                    }
                }
            }
            processes[id] = nil
            if jobs.first(where: { $0.id == id })?.state == .cancelled { return }
            guard result.status == 0 else { throw DownloadError.message(Self.errorMessage(result.err)) }

            let title = jobs.first(where: { $0.id == id })?.title ?? "Download"
            let saved = try Self.keep(from: work, title: title)
            Store.shared.add([saved], to: playlist)
            update(id) {
                $0.state = .done
                $0.progress = 1
            }
        } catch {
            processes[id] = nil
            update(id) { $0.state = .failed(error.localizedDescription) }
        }
    }

    /// Audio-only formats Soundpad plays without converting: stereo (or mono)
    /// AAC-LC or MP3. YouTube always has AAC-LC stereo (format 140), but some
    /// videos also offer 5.1 or HE-AAC tracks with a higher bitrate, which
    /// "bestaudio" would pick and the decoder can't play.
    nonisolated static let formats = [
        "bestaudio[acodec^=mp4a.40.2][audio_channels<=?2]",
        "bestaudio[acodec=aac][audio_channels<=?2]",
        "bestaudio[ext=mp3]",
        "bestaudio[acodec=mp3]",
    ].joined(separator: "/")

    /// yt-dlp options: one item, audio only (`formats`), plus the thumbnail
    /// as cover art.
    nonisolated static func arguments(for target: String, into dir: URL) -> [String] {
        [
            "--no-playlist", "--playlist-items", "1",
            "-f", formats,
            "--write-thumbnail", "--no-mtime", "--no-overwrites", "--no-warnings",
            // --print turns on quiet mode, which hides progress unless asked for.
            "--progress", "--newline", "--progress-template", "download:SP-PROGRESS %(progress._percent_str)s %(progress.fragment_index)s/%(progress.fragment_count)s",
            "--no-simulate", "--print", "before_dl:SP-TITLE %(title)s",
            "-P", dir.path, "-o", "audio.%(ext)s", "-o", "thumbnail:cover.%(ext)s",
            "--", target,
        ]
    }

    /// Reads yt-dlp's title and progress lines.
    nonisolated static func parse(_ line: String) -> (title: String?, progress: Double?) {
        if line.hasPrefix("SP-TITLE ") {
            let t = line.dropFirst("SP-TITLE ".count).trimmingCharacters(in: .whitespaces)
            return (t.isEmpty ? nil : t, nil)
        }
        if line.hasPrefix("SP-PROGRESS ") {
            // "  42.5% NA/NA" for a plain download; "10.0% 1/5" for a stream in
            // fragments (SoundCloud), where the percentage is a jumpy estimate
            // but the fragment count is exact.
            let parts = line.dropFirst("SP-PROGRESS ".count).split(separator: " ")
            if let frag = parts.last?.split(separator: "/"), frag.count == 2,
               let i = Double(frag[0]), let n = Double(frag[1]), n > 0 {
                return (nil, min(max(i / n, 0), 1))
            }
            let percent = parts.first.map { $0.replacingOccurrences(of: "%", with: "") }
            return (nil, percent.flatMap(Double.init).map { min(max($0 / 100, 0), 1) })
        }
        return (nil, nil)
    }

    /// The last "ERROR:" line yt-dlp printed, without its prefixes.
    nonisolated static func errorMessage(_ stderr: String) -> String {
        guard let line = stderr.split(separator: "\n").last(where: { $0.hasPrefix("ERROR:") }) else {
            return String(localized: "The download failed.")
        }
        var msg = line.dropFirst("ERROR:".count).trimmingCharacters(in: .whitespaces)
        // "[youtube] dQw4w9WgXcQ: Video unavailable" → "Video unavailable"
        if msg.hasPrefix("["), let r = msg.range(of: ": ") { msg = String(msg[r.upperBound...]) }
        return msg
    }

    /// Moves the downloaded audio (and its cover) into the downloads folder as
    /// "<title>.<ext>", never overwriting an existing file.
    nonisolated static func keep(from work: URL, title: String) throws -> URL {
        let files = (try? FileManager.default.contentsOfDirectory(at: work, includingPropertiesForKeys: nil)) ?? []
        guard let audio = files.first(where: { $0.lastPathComponent.hasPrefix("audio.") }) else {
            throw DownloadError.message(String(localized: "The download failed."))
        }
        let folder = downloadsFolder
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let dest = uniqueURL(in: folder, base: fileName(for: title), ext: audio.pathExtension)
        try FileManager.default.moveItem(at: audio, to: dest)
        if let cover = files.first(where: { $0.lastPathComponent.hasPrefix("cover.") }) {
            let art = dest.deletingPathExtension().appendingPathExtension(cover.pathExtension)
            try? FileManager.default.moveItem(at: cover, to: art)
        }
        return dest
    }

    /// A safe file name for a title.
    nonisolated static func fileName(for title: String) -> String {
        let cleaned = title
            .replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: " -")
            .components(separatedBy: .controlCharacters).joined()
            .trimmingCharacters(in: .whitespacesAndNewlines.union(CharacterSet(charactersIn: ".")))
        let name = String(cleaned.prefix(120))
        return name.isEmpty ? "Download" : name
    }

    /// "Name.m4a", or "Name 2.m4a", "Name 3.m4a"… when the name is taken by
    /// any audio or cover file, so a cover never belongs to two sounds.
    nonisolated static func uniqueURL(in folder: URL, base: String, ext: String) -> URL {
        let exts = Set([ext.lowercased(), "m4a", "mp3", "aac", "jpg", "jpeg", "png", "webp"])
        var n = 1
        while true {
            let name = n == 1 ? base : "\(base) \(n)"
            let taken = exts.contains { FileManager.default.fileExists(atPath: folder.appending(path: "\(name).\($0)").path) }
            if !taken { return folder.appending(path: "\(name).\(ext)") }
            n += 1
        }
    }

    // MARK: Spotify

    struct SpotifySong: Equatable {
        let title: String
        let artist: String
        var display: String { artist.isEmpty ? title : "\(artist) – \(title)" }
        var search: String { artist.isEmpty ? title : "\(artist) - \(title)" }
    }

    /// Reads a Spotify song's public title and artist from its web page.
    nonisolated static func spotifySong(_ link: String) async throws -> SpotifySong {
        guard let url = URL(string: link) else { throw DownloadError.message(String(localized: "The link isn't valid.")) }
        var request = URLRequest(url: url)
        request.setValue("Mozilla/5.0 (Macintosh)", forHTTPHeaderField: "User-Agent")
        let (data, response) = try await URLSession.shared.data(for: request)
        // spotify.link short links redirect to open.spotify.com/track/…
        guard response.url?.path().contains("/track/") == true else {
            throw DownloadError.message(String(localized: "Only single Spotify songs can be downloaded, not albums or playlists."))
        }
        guard let song = parseSpotifyPage(String(decoding: data, as: UTF8.self)) else {
            throw DownloadError.message(String(localized: "Couldn't read the song from Spotify."))
        }
        return song
    }

    /// og:title is the song; og:description starts with the artist
    /// ("Artist · Song · 2020").
    nonisolated static func parseSpotifyPage(_ html: String) -> SpotifySong? {
        func meta(_ property: String) -> String? {
            let pattern = #"<meta[^>]+property="\#(property)"[^>]+content="([^"]*)""#
            guard let re = try? NSRegularExpression(pattern: pattern),
                  let m = re.firstMatch(in: html, range: NSRange(html.startIndex..., in: html)),
                  let r = Range(m.range(at: 1), in: html)
            else { return nil }
            return decodeEntities(String(html[r]))
        }
        guard let title = meta("og:title"), !title.isEmpty else { return nil }
        let artist = meta("og:description")?
            .components(separatedBy: " · ").first?
            .trimmingCharacters(in: .whitespaces) ?? ""
        return SpotifySong(title: title, artist: artist)
    }

    nonisolated private static func decodeEntities(_ s: String) -> String {
        var out = s
        for (entity, char) in [("&amp;", "&"), ("&quot;", "\""), ("&#39;", "'"), ("&#x27;", "'"), ("&lt;", "<"), ("&gt;", ">")] {
            out = out.replacingOccurrences(of: entity, with: char)
        }
        return out
    }

    // MARK: Processes

    /// Runs a program, passing each output line to `onLine` as it arrives.
    nonisolated static func run(
        _ exe: String, _ args: [String],
        started: (@Sendable (Process) -> Void)? = nil,
        onLine: (@Sendable (String) -> Void)? = nil
    ) async -> (status: Int32, out: String, err: String) {
        await withCheckedContinuation { done in
            let p = Process()
            p.executableURL = URL(fileURLWithPath: exe)
            p.arguments = args
            let out = Pipe(), err = Pipe()
            p.standardOutput = out
            p.standardError = err
            let collected = Collected()
            out.fileHandleForReading.readabilityHandler = { h in
                let data = h.availableData
                for line in collected.appendOut(data) { onLine?(line) }
            }
            err.fileHandleForReading.readabilityHandler = { h in collected.appendErr(h.availableData) }
            p.terminationHandler = { p in
                out.fileHandleForReading.readabilityHandler = nil
                err.fileHandleForReading.readabilityHandler = nil
                // Whatever arrived after the last read.
                for line in collected.appendOut(out.fileHandleForReading.readDataToEndOfFile(), flush: true) { onLine?(line) }
                collected.appendErr(err.fileHandleForReading.readDataToEndOfFile())
                done.resume(returning: (p.terminationStatus, collected.out, collected.err))
            }
            do {
                try p.run()
                started?(p)
            } catch {
                done.resume(returning: (-1, "", error.localizedDescription))
            }
        }
    }

    /// Output collected from a process's pipes (written from their threads).
    private final class Collected: @unchecked Sendable {
        private let lock = NSLock()
        private var outData = Data(), errData = Data(), partial = ""
        var out: String { lock.withLock { String(decoding: outData, as: UTF8.self) } }
        var err: String { lock.withLock { String(decoding: errData, as: UTF8.self) } }

        /// Adds output and returns the complete lines it finished.
        func appendOut(_ data: Data, flush: Bool = false) -> [String] {
            lock.withLock {
                outData.append(data)
                partial += String(decoding: data, as: UTF8.self)
                var lines = partial.components(separatedBy: "\n")
                partial = flush ? "" : lines.removeLast()
                return lines.filter { !$0.isEmpty }
            }
        }

        func appendErr(_ data: Data) { lock.withLock { errData.append(data) } }
    }
}

enum DownloadError: LocalizedError {
    case message(String)
    var errorDescription: String? { if case .message(let m) = self { m } else { nil } }
}

import AppKit
import SwiftUI

/// Toolbar popover: paste a link, download its audio, watch the progress.
struct DownloadPopover: View {
    @Environment(Store.self) private var store
    @State private var link = ""
    @FocusState private var focused: Bool
    private var downloader: MediaDownloader { .shared }

    private var site: MediaDownloader.Site? { MediaDownloader.site(for: link) }
    private var ready: Bool { if case .ready = downloader.tool { true } else { false } }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Download from a Link").font(.headline)
            HStack {
                TextField("YouTube, YouTube Music, SoundCloud or Spotify link", text: $link)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused)
                    .onSubmit(start)
                Button("Download", action: start)
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(site == nil || !ready)
            }
            hint
            toolStatus
            if !downloader.jobs.isEmpty {
                Divider()
                ScrollView {
                    VStack(spacing: 10) {
                        ForEach(downloader.jobs) { JobRow(job: $0) }
                    }
                }
                .frame(maxHeight: 220)
                .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Button("Show in Finder") { NSWorkspace.shared.open(MediaDownloader.downloadsFolder) }
                    Spacer()
                    Button("Clear Finished") { downloader.clearFinished() }
                        .disabled(!downloader.jobs.contains { $0.state != .running })
                }
                .controlSize(.small)
            }
            Text("Only download audio you have the right to use.")
                .font(.caption2).foregroundStyle(.tertiary)
        }
        .padding(16)
        .frame(width: 440)
        .onAppear {
            // A supported link on the clipboard is most likely what the user wants.
            if link.isEmpty, let s = NSPasteboard.general.string(forType: .string),
               MediaDownloader.site(for: s) != nil {
                link = s.trimmingCharacters(in: .whitespacesAndNewlines)
            }
            focused = true
        }
    }

    @ViewBuilder private var hint: some View {
        let destination = store.activePlaylist.map { String(localized: "Adds it to “\($0.name)”.") }
            ?? String(localized: "Adds it to All Sounds.")
        Group {
            if link.trimmingCharacters(in: .whitespaces).isEmpty {
                Text(destination)
            } else if let site {
                if site == .spotify {
                    Text("Spotify: the same song is found and downloaded from YouTube. \(destination)")
                } else {
                    Text("\(site.name) link. \(destination)")
                }
            } else {
                Label("This isn't a supported link.", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder private var toolStatus: some View {
        switch downloader.tool {
        case .ready:
            EmptyView()
        case .installing, .missing:
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Setting up yt-dlp…").font(.caption).foregroundStyle(.secondary)
            }
        case .failed(let message):
            HStack {
                Label(message, systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(.orange)
                Spacer()
                Button("Try Again") { Task { await downloader.prepare() } }
                    .controlSize(.small)
            }
        }
    }

    private func start() {
        guard site != nil, ready else { return }
        downloader.download(link, into: store.activePlaylist?.id)
        link = ""
    }
}

private struct JobRow: View {
    let job: MediaDownloader.Job

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            icon.frame(width: 18)
            VStack(alignment: .leading, spacing: 4) {
                Text(job.title).lineLimit(1).truncationMode(.middle)
                switch job.state {
                case .running:
                    if let p = job.progress { ProgressView(value: p) } else { ProgressView().progressViewStyle(.linear) }
                case .done:
                    Text("Added").font(.caption).foregroundStyle(.secondary)
                case .failed(let message):
                    Text(message).font(.caption).foregroundStyle(.red).lineLimit(3)
                case .cancelled:
                    Text("Cancelled").font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
            if job.state == .running {
                Button { MediaDownloader.shared.cancel(job.id) } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .help("Cancel")
            }
        }
    }

    @ViewBuilder private var icon: some View {
        switch job.state {
        case .running: Image(systemName: "arrow.down.circle").foregroundStyle(.tint)
        case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .failed: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .cancelled: Image(systemName: "xmark.circle").foregroundStyle(.secondary)
        }
    }
}

/// Settings ▸ Download from Links.
struct DownloadsSection: View {
    @AppStorage(MediaDownloader.enabledKey) private var enabled = false
    @State private var confirm = false
    private var downloader: MediaDownloader { .shared }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Toggle("Download audio from links", isOn: Binding(
                get: { enabled },
                set: { on in if on { confirm = true } else { enabled = false } }))
            Text("Adds a download button to the toolbar: paste a YouTube, YouTube Music, SoundCloud or Spotify link to get its audio. Off by default.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .alert("Download audio from links?", isPresented: $confirm) {
            Button("Turn On") {
                enabled = true
                Task { await downloader.prepare() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Soundpad uses yt-dlp, a free open-source downloader. It's downloaded from yt-dlp's official GitHub page (about 35 MB) and kept up to date. Spotify songs are found and downloaded from YouTube.\n\nOnly download audio you have the right to use. Downloading can be against a site's terms of service.")
        }
        if enabled {
            LabeledContent {
                HStack(spacing: 8) {
                    switch downloader.tool {
                    case .ready(_, let version, let managed):
                        Text(managed ? version : String(localized: "\(version) (installed on this Mac)"))
                            .foregroundStyle(.secondary)
                        if managed {
                            Button("Update") { Task { await downloader.updateIfDue(force: true) } }
                        }
                    case .installing, .missing:
                        ProgressView().controlSize(.small)
                        Text("Setting up yt-dlp…").foregroundStyle(.secondary)
                    case .failed(let message):
                        Text(message).foregroundStyle(.orange).lineLimit(2)
                        Button("Try Again") { Task { await downloader.prepare() } }
                    }
                }
            } label: {
                Text(verbatim: "yt-dlp")
            }
            LabeledContent("Saved to") {
                Button("Show in Finder") {
                    try? FileManager.default.createDirectory(at: MediaDownloader.downloadsFolder, withIntermediateDirectories: true)
                    NSWorkspace.shared.open(MediaDownloader.downloadsFolder)
                }
            }
        }
    }
}

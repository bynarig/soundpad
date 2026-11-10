import AppKit
import AVFoundation
import SwiftUI

// MARK: - View style

/// How the sound list is shown, like Finder's View ▸ as Icons / as List.
enum ViewStyle: String, CaseIterable, Identifiable {
    case icons, list
    var id: Self { self }

    var title: LocalizedStringKey {
        switch self {
        case .icons: "as Icons"
        case .list: "as List"
        }
    }

    var symbol: String {
        switch self {
        case .icons: "square.grid.2x2"
        case .list: "list.bullet"
        }
    }
}

enum IconSize {
    static let range: ClosedRange<Double> = 64...224
    static let standard: Double = 112
}

// MARK: - Artwork

/// Cover art for a sound: the picture embedded in the file (ID3, MP4, AIFF…),
/// or a cover image next to it (cover.jpg, folder.jpg…). Cached, downscaled.
@MainActor
final class ArtworkCache {
    static let shared = ArtworkCache()

    private let images = NSCache<NSString, NSImage>()
    /// Paths known to have no artwork, so they aren't looked up again.
    private var missing = Set<String>()
    private var pending: [String: Task<NSImage?, Never>] = [:]

    private init() { images.countLimit = 400 }

    func cached(_ path: String) -> NSImage? { images.object(forKey: path as NSString) }

    func image(for path: String) async -> NSImage? {
        if let img = cached(path) { return img }
        if missing.contains(path) { return nil }
        if let task = pending[path] { return await task.value }
        let task = Task.detached(priority: .utility) { await Self.load(path) }
        pending[path] = task
        let img = await task.value
        pending[path] = nil
        if let img { images.setObject(img, forKey: path as NSString) } else { missing.insert(path) }
        return img
    }

    private nonisolated static func load(_ path: String) async -> NSImage? {
        let url = URL(fileURLWithPath: path)
        if let data = await embedded(url), let img = thumbnail(data) { return img }
        let dir = url.deletingLastPathComponent()
        for name in ["cover", "folder", "front", "album", "artwork"] {
            for ext in ["jpg", "jpeg", "png", "webp"] {
                let f = dir.appendingPathComponent("\(name).\(ext)")
                if let data = try? Data(contentsOf: f), let img = thumbnail(data) { return img }
            }
        }
        return nil
    }

    private nonisolated static func embedded(_ url: URL) async -> Data? {
        let asset = AVURLAsset(url: url)
        guard let items = try? await asset.load(.metadata) else { return nil }
        let art = AVMetadataItem.metadataItems(from: items, filteredByIdentifier: .commonIdentifierArtwork).first
            ?? items.first { $0.commonKey == .commonKeyArtwork }
        guard let art else { return nil }
        return try? await art.load(.dataValue)
    }

    /// Decode and shrink to at most 512 px so big embedded covers don't use much memory.
    private nonisolated static func thumbnail(_ data: Data) -> NSImage? {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 512,
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary) else { return nil }
        return NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height))
    }
}

/// Square cover art, or a tinted placeholder with a note when there is none.
struct ArtworkView: View {
    let sound: Sound
    var cornerRadius: CGFloat = 10
    @State private var image: NSImage?

    var body: some View {
        ZStack {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fill)
            } else {
                placeholder
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .clipShape(.rect(cornerRadius: cornerRadius, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                .strokeBorder(.primary.opacity(0.08), lineWidth: 0.5)
        }
        .task(id: sound.path) {
            image = ArtworkCache.shared.cached(sound.path)
            if image == nil { image = await ArtworkCache.shared.image(for: sound.path) }
        }
    }

    private var placeholder: some View {
        // A stable color per sound, so tiles are easy to tell apart.
        let hash = sound.path.utf8.reduce(UInt64(1469598103934665603)) { ($0 ^ UInt64($1)) &* 1099511628211 }
        let hue = (Double(hash % 1000) * 0.618033988749895).truncatingRemainder(dividingBy: 1)
        return GeometryReader { geo in
            ZStack {
                LinearGradient(
                    colors: [Color(hue: hue, saturation: 0.45, brightness: 0.62),
                             Color(hue: hue, saturation: 0.55, brightness: 0.38)],
                    startPoint: .topLeading, endPoint: .bottomTrailing)
                Image(systemName: sound.isMissing ? "questionmark.folder.fill" : sound.isFailed ? "exclamationmark.triangle.fill" : "music.note")
                    .font(.system(size: geo.size.width * 0.34, weight: .medium))
                    .foregroundStyle(.white.opacity(0.85))
            }
        }
    }
}

// MARK: - Icon grid

/// Finder-style icon view: click to select, ⌘-click to toggle, ⇧-click for a
/// range, double-click (or Return) to play, arrow keys to move.
struct SoundGrid<Menu: View>: View {
    @Environment(Store.self) private var store
    let sounds: [Sound]
    @Binding var selection: Set<UInt64>
    let iconSize: Double
    let dragIDs: (UInt64) -> [UInt64]
    let onDelete: () -> Void
    @ViewBuilder let menu: (Set<UInt64>) -> Menu

    @State private var anchor: UInt64?
    @State private var width: CGFloat = 600
    @FocusState private var focused: Bool

    private let spacing: CGFloat = 12
    private let padding: CGFloat = 20
    private var tileWidth: CGFloat { iconSize + 20 }
    private var columns: Int { columns(for: width) }
    private func columns(for width: CGFloat) -> Int {
        max(1, Int((width - padding * 2 + spacing) / (tileWidth + spacing)))
    }

    var body: some View {
        // Size the grid from the space we're offered, never from the grid itself:
        // measuring the grid made a too-wide grid keep its columns and push the
        // whole window layout sideways when the sidebar grew.
        GeometryReader { geo in
            grid(columns: columns(for: geo.size.width))
                .onAppear { width = geo.size.width }
                .onChange(of: geo.size.width) { _, w in width = w }
        }
    }

    private func grid(columns: Int) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                // A fixed number of fixed-width columns, pinned to the left like
                // Finder: icons only move when a whole column fits or doesn't.
                LazyVGrid(columns: Array(repeating: GridItem(.fixed(tileWidth), spacing: spacing, alignment: .top), count: columns),
                          alignment: .leading, spacing: spacing) {
                    ForEach(sounds) { sound in
                        SoundTile(sound: sound, iconSize: iconSize, selected: selection.contains(sound.id))
                            .frame(width: tileWidth)
                            .contentShape(.rect(cornerRadius: 10))
                            .onTapGesture(count: 2) { if sound.isReady { store.play(sound.id) } }
                            .simultaneousGesture(TapGesture().onEnded {
                                focused = true
                                click(sound.id)
                            })
                            .contextMenu { menu(selection.contains(sound.id) ? selection : [sound.id]) }
                            .draggable(DroppedItem.payload(dragIDs(sound.id)))
                            .id(sound.id)
                    }
                }
                .padding(padding)
                .frame(maxWidth: .infinity, alignment: .topLeading)
            }
            .background {
                // Clicking empty space deselects, like Finder.
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture {
                        focused = true
                        selection = []
                    }
            }
            .focusable()
            .focused($focused)
            .focusEffectDisabled()
            .onDeleteCommand(perform: onDelete)
            .onKeyPress(.return) {
                if let id = orderedSelection.first { store.play(id) }
                return .handled
            }
            .onKeyPress(.space) {
                store.togglePause()
                return .handled
            }
            .onKeyPress(keys: [.leftArrow, .rightArrow, .upArrow, .downArrow]) { press in
                let step = switch press.key {
                case .leftArrow: -1
                case .rightArrow: 1
                case .upArrow: -columns
                default: columns
                }
                guard let id = move(by: step, extend: press.modifiers.contains(.shift)) else { return .ignored }
                withAnimation(.smooth(duration: 0.2)) { proxy.scrollTo(id) }
                return .handled
            }
            .onExitCommand { store.stop() }
            .onAppear { focused = true }
        }
    }

    private var orderedSelection: [UInt64] { sounds.map(\.id).filter(selection.contains) }

    private func click(_ id: UInt64) {
        let mods = NSEvent.modifierFlags
        if mods.contains(.command) {
            if selection.contains(id) { selection.remove(id) } else { selection.insert(id) }
            anchor = id
        } else if mods.contains(.shift), let anchor, let range = range(anchor, id) {
            selection.formUnion(range)
        } else {
            selection = [id]
            anchor = id
        }
    }

    private func range(_ a: UInt64, _ b: UInt64) -> [UInt64]? {
        let ids = sounds.map(\.id)
        guard let i = ids.firstIndex(of: a), let j = ids.firstIndex(of: b) else { return nil }
        return Array(ids[min(i, j)...max(i, j)])
    }

    /// Moves the selection with the arrow keys; returns the newly focused sound.
    private func move(by step: Int, extend: Bool) -> UInt64? {
        let ids = sounds.map(\.id)
        guard !ids.isEmpty else { return nil }
        let current = anchor.flatMap { ids.firstIndex(of: $0) } ?? orderedSelection.last.flatMap { ids.firstIndex(of: $0) }
        let target: Int
        if let current {
            let t = current + step
            guard t >= 0, t < ids.count else { return nil }
            target = t
        } else {
            target = step > 0 ? 0 : ids.count - 1
        }
        let id = ids[target]
        if extend { selection.insert(id) } else { selection = [id] }
        anchor = id
        return id
    }
}

struct SoundTile: View {
    @Environment(Store.self) private var store
    let sound: Sound
    let iconSize: Double
    let selected: Bool

    var body: some View {
        let playing = store.playback.soundID == sound.id
        VStack(spacing: 6) {
            ArtworkView(sound: sound, cornerRadius: max(6, iconSize * 0.08))
                .frame(width: iconSize, height: iconSize)
                .overlay(alignment: .topLeading) {
                    if let label = sound.hotkeyLabel {
                        Keycap(label).scaleEffect(0.85, anchor: .topLeading).padding(6)
                    }
                }
                .overlay(alignment: .bottom) {
                    if playing { TileProgress().padding(8) }
                }
                .overlay {
                    if playing {
                        Image(systemName: store.playback.paused ? "pause.fill" : "speaker.wave.2.fill")
                            .font(.system(size: iconSize * 0.18, weight: .semibold))
                            .foregroundStyle(.white)
                            .symbolEffect(.variableColor.iterative, isActive: !store.playback.paused)
                            .padding(iconSize * 0.1)
                            .background(.black.opacity(0.35), in: .circle)
                    } else if sound.isLoading {
                        ProgressView().controlSize(.small)
                    } else if sound.isMissing {
                        // Cover art may still be cached; badge it so it's clear.
                        Image(systemName: "questionmark.folder.fill")
                            .font(.system(size: iconSize * 0.18, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(iconSize * 0.1)
                            .background(.orange.opacity(0.85), in: .circle)
                    }
                }
                .opacity(sound.isMissing ? 0.6 : 1)
                .padding(4)
                .background(selected ? Color.primary.opacity(0.12) : .clear, in: .rect(cornerRadius: max(8, iconSize * 0.1)))

            VStack(spacing: 1) {
                Text(sound.name)
                    .font(.callout.weight(playing ? .semibold : .regular))
                    .foregroundStyle(selected ? AnyShapeStyle(.white) : sound.isFailed ? AnyShapeStyle(.red) : sound.isMissing ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(selected ? Color.accentColor : .clear, in: .rect(cornerRadius: 4))
                if let d = sound.duration {
                    Text(formatLength(d))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
        }
        .help(sound.tooltip)
        .animation(.smooth(duration: 0.2), value: playing)
    }
}

/// Thin progress bar over the playing tile's artwork.
struct TileProgress: View {
    @Environment(Store.self) private var store
    var body: some View {
        GeometryReader { geo in
            Capsule().fill(.black.opacity(0.35))
                .overlay(alignment: .leading) {
                    Capsule().fill(.white).frame(width: geo.size.width * store.playback.fraction)
                }
        }
        .frame(height: 4)
    }
}

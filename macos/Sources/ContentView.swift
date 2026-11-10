import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 170, ideal: 200, max: 300)
        } detail: {
            SoundList()
        }
        .frame(minWidth: 760, minHeight: 460)
    }
}

// MARK: - Drag & drop

/// Something dropped on the window or a playlist: audio files from Finder, or
/// sounds dragged out of the list.
enum DroppedItem: Transferable {
    case file(URL)
    case sounds([UInt64])

    static let prefix = "soundpad-sounds:"

    static var transferRepresentation: some TransferRepresentation {
        ProxyRepresentation(importing: { (url: URL) in DroppedItem.file(url) })
        ProxyRepresentation(importing: { (text: String) in
            guard text.hasPrefix(prefix) else { throw CocoaError(.fileReadUnsupportedScheme) }
            return DroppedItem.sounds(text.dropFirst(prefix.count).split(separator: ",").compactMap { UInt64($0) })
        })
    }

    /// Drag payload for a set of sounds.
    static func payload(_ ids: [UInt64]) -> String {
        prefix + ids.map(String.init).joined(separator: ",")
    }
}

extension Store {
    /// Handle a drop onto `playlist` (nil = the library). Returns whether anything was accepted.
    @discardableResult
    func accept(_ items: [DroppedItem], into playlist: UInt64?) -> Bool {
        var urls: [URL] = []
        var ids: [UInt64] = []
        for item in items {
            switch item {
            case .file(let u): urls.append(u)
            case .sounds(let s): ids.append(contentsOf: s)
            }
        }
        if !urls.isEmpty { add(urls, to: playlist) }
        if let playlist, !ids.isEmpty { addToPlaylist(playlist, ids) }
        return !urls.isEmpty || (playlist != nil && !ids.isEmpty)
    }
}

// MARK: - Sidebar

struct Sidebar: View {
    @Environment(Store.self) private var store
    @State private var renaming: Playlist?
    @State private var dropTarget: UInt64?

    /// 0 stands for "All Sounds" in the selection.
    private var selection: Binding<UInt64?> {
        Binding(
            get: { store.state.activePlaylist ?? 0 },
            set: { store.showPlaylist(($0 ?? 0) == 0 ? nil : $0) })
    }

    var body: some View {
        List(selection: selection) {
            row(title: String(localized: "All Sounds"), icon: "square.grid.2x2", count: store.state.sounds.count, id: 0)
                .tag(UInt64(0))
            Section("Playlists") {
                ForEach(store.state.playlists) { pl in
                    row(title: pl.name, icon: "music.note.list", count: pl.sounds.count, id: pl.id)
                        .tag(pl.id)
                        .contextMenu {
                            Button("Play", systemImage: "play.fill") {
                                store.showPlaylist(pl.id)
                                if let first = pl.sounds.first(where: { store.sound($0)?.isReady == true }) {
                                    store.play(first)
                                }
                            }
                            .disabled(pl.sounds.isEmpty)
                            Button("Rename…", systemImage: "pencil") { renaming = pl }
                            Divider()
                            Button("Delete Playlist", systemImage: "trash", role: .destructive) {
                                store.deletePlaylist(pl.id)
                            }
                        }
                }
                if store.state.playlists.isEmpty {
                    Text("Drop audio files on ＋ New Playlist, or right-click sounds › Add to Playlist")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(4)
                        .fixedSize(horizontal: false, vertical: true)
                        .selectionDisabled()
                }
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            Button { store.createPlaylist() } label: {
                Label("New Playlist", systemImage: "plus")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .background(dropTarget == UInt64.max ? Color.accentColor.opacity(0.2) : .clear)
            .dropDestination(for: DroppedItem.self) { items, _ in
                // Dropping here makes a new playlist from the dropped sounds/files.
                store.createPlaylist()
                guard let id = store.state.activePlaylist else { return false }
                return store.accept(items, into: id)
            } isTargeted: { dropTarget = $0 ? UInt64.max : nil }
            .help("New playlist — or drop sounds or files here to make one")
        }
        .sheet(item: $renaming) { RenamePlaylistSheet(playlist: $0) }
    }

    private func row(title: String, icon: String, count: Int, id: UInt64) -> some View {
        Label(title, systemImage: icon)
            .badge(count)
            .lineLimit(1)
            .padding(.vertical, 1)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.accentColor.opacity(dropTarget == id ? 0.25 : 0))
                    .padding(-4))
            .dropDestination(for: DroppedItem.self) { items, _ in
                store.accept(items, into: id == 0 ? nil : id)
            } isTargeted: { dropTarget = $0 ? id : (dropTarget == id ? nil : dropTarget) }
    }
}

struct RenamePlaylistSheet: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let playlist: Playlist
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Rename Playlist").font(.headline)
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .onSubmit(save)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Rename", action: save)
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 340)
        .onAppear { name = playlist.name }
    }

    private func save() {
        store.renamePlaylist(playlist.id, name)
        dismiss()
    }
}

// MARK: - Sound list

struct SoundList: View {
    @Environment(Store.self) private var store
    @State private var selection: Set<UInt64> = []
    @State private var search = ""
    @State private var recording: HotkeyTarget?
    @State private var renaming: Sound?
    @State private var showImporter = false
    @State private var dropTargeted = false

    private var playlist: Playlist? { store.activePlaylist }

    private var filtered: [Sound] {
        let q = search.trimmingCharacters(in: .whitespaces).lowercased()
        let all = store.visibleSounds
        return q.isEmpty ? all : all.filter { $0.name.lowercased().contains(q) }
    }

    var body: some View {
        list
            // Empty states size to their content; fill the window so the player bar stays at the bottom.
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationTitle(playlist?.name ?? String(localized: "All Sounds"))
            .navigationSubtitle(subtitle)
            .searchable(text: $search, placement: .toolbar, prompt: "Search sounds")
            .toolbar { toolbar }
            .safeAreaInset(edge: .top, spacing: 0) {
                VStack(spacing: 0) {
                    DriverUpdateBanner()
                    SpeakerWarningBanner()
                    SetupBanner()
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                PlayerBar().padding(.horizontal, 14).padding(.bottom, 14).padding(.top, 6)
            }
            .overlay(alignment: .top) { ToastStack().padding(.top, 10) }
            .overlay { if dropTargeted { DropOverlay(playlist: playlist?.name) } }
            .dropDestination(for: DroppedItem.self) { items, _ in
                // Files from Finder; sounds dragged within the list are reorders.
                let files = items.filter { if case .file = $0 { true } else { false } }
                return !files.isEmpty && store.accept(files, into: playlist?.id)
            } isTargeted: { dropTargeted = $0 }
            .fileImporter(
                isPresented: $showImporter, allowedContentTypes: [.audio, .folder], allowsMultipleSelection: true
            ) { result in
                if case .success(let urls) = result { store.add(urls, to: playlist?.id) }
            }
            .sheet(item: $recording) { HotkeyRecorder(target: $0) }
            .sheet(item: $renaming) { RenameSheet(sound: $0) }
            .focusedSceneValue(\.addSoundsAction) { showImporter = true }
            .onChange(of: store.state.activePlaylist) { selection = [] }
    }

    private var subtitle: String {
        let n = store.visibleSounds.count
        return n == 1 ? String(localized: "1 sound") : String(localized: "\(n) sounds")
    }

    @ViewBuilder private var list: some View {
        if store.state.sounds.isEmpty {
            ContentUnavailableView {
                Label("No Sounds Yet", systemImage: "waveform.badge.plus")
            } description: {
                Text("Drop audio files or folders here, or add them with ＋.")
            } actions: {
                Button("Add Sounds…") { showImporter = true }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
            }
        } else if let playlist, playlist.sounds.isEmpty {
            ContentUnavailableView {
                Label("Empty Playlist", systemImage: "music.note.list")
            } description: {
                Text("Drop audio files here, or drag sounds from All Sounds onto “\(playlist.name)” in the sidebar.")
            } actions: {
                Button("Add Sounds…") { showImporter = true }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
            }
        } else {
            List(selection: $selection) {
                ForEach(Array(filtered.enumerated()), id: \.element.id) { index, sound in
                    SoundRow(sound: sound, number: index + 1)
                        .tag(sound.id)
                        .draggable(DroppedItem.payload(dragIDs(for: sound.id)))
                }
                .onMove(perform: search.isEmpty ? move : nil)
            }
            .listStyle(.inset)
            .alternatingRowBackgrounds()
            .contextMenu(forSelectionType: UInt64.self) { ids in
                contextMenu(ids)
            } primaryAction: { ids in
                if let id = ids.first { store.play(id) }
            }
            .onDeleteCommand { removeSelection(selection) }
            .onKeyPress(.space) {
                store.togglePause()
                return .handled
            }
            .onExitCommand { store.stop() }
        }
    }

    /// Dragging a selected row drags the whole selection.
    private func dragIDs(for id: UInt64) -> [UInt64] {
        selection.contains(id) ? filtered.map(\.id).filter(selection.contains) : [id]
    }

    private func removeSelection(_ ids: Set<UInt64>) {
        if let playlist { store.removeFromPlaylist(playlist.id, ids) } else { store.remove(ids) }
        selection.subtract(ids)
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Button { showImporter = true } label: { Label("Add Sounds", systemImage: "plus") }
                .help(playlist.map { String(localized: "Add sounds to “\($0.name)” (⌘O)") } ?? String(localized: "Add sounds (⌘O)"))
        }
        ToolbarItem(placement: .primaryAction) {
            SettingsLink { Label("Settings", systemImage: "gearshape") }
                .help("Settings (⌘,)")
        }
        if Updater.shared.availableUpdate != nil {
            ToolbarItem(placement: .primaryAction) {
                SettingsLink {
                    Label("Update Available", systemImage: "arrow.down.circle.fill")
                        .labelStyle(.titleAndIcon)
                }
                .help("Update Available")
            }
        }
    }

    @ViewBuilder private func contextMenu(_ ids: Set<UInt64>) -> some View {
        if ids.count == 1, let id = ids.first, let s = store.sound(id) {
            Button("Play", systemImage: "play.fill") { store.play(id) }
                .disabled(!s.isReady)
            Divider()
            Button(s.hotkey == nil ? String(localized: "Set Hotkey…") : String(localized: "Change Hotkey…"), systemImage: "keyboard") {
                recording = .sound(id)
            }
            if s.hotkey != nil {
                Button("Clear Hotkey", systemImage: "keyboard.badge.ellipsis") { store.clearHotkey(.sound(id)) }
            }
            Button("Rename…", systemImage: "pencil") { renaming = s }
            Button("Show in Finder", systemImage: "folder") { store.reveal(id) }
            if s.isFailed {
                Button("Retry Loading", systemImage: "arrow.clockwise") { store.retry(id) }
            }
            Divider()
        }
        if !ids.isEmpty {
            let ordered = filtered.map(\.id).filter(ids.contains)
            Menu("Add to Playlist", systemImage: "text.badge.plus") {
                ForEach(store.state.playlists.filter { $0.id != playlist?.id }) { pl in
                    Button(pl.name) { store.addToPlaylist(pl.id, ordered) }
                }
                if store.state.playlists.contains(where: { $0.id != playlist?.id }) { Divider() }
                Button("New Playlist from Selection", systemImage: "plus") { store.createPlaylist(with: ordered) }
            }
            if let playlist {
                Button(ids.count == 1 ? String(localized: "Remove from Playlist") : String(localized: "Remove \(ids.count) from Playlist"),
                       systemImage: "minus.circle") {
                    store.removeFromPlaylist(playlist.id, ids)
                    selection.subtract(ids)
                }
            }
            Button(ids.count == 1 ? String(localized: "Delete from Library") : String(localized: "Delete \(ids.count) from Library"),
                   systemImage: "trash", role: .destructive) {
                store.remove(ids)
                selection.subtract(ids)
            }
        }
    }

    private func move(from source: IndexSet, to destination: Int) {
        guard let from = source.first else { return }
        let sounds = store.visibleSounds
        let id = sounds[from].id
        let to = destination > from ? destination - 1 : destination
        if let playlist { store.moveInPlaylist(playlist.id, id, to: to) } else { store.move(id, to: to) }
    }
}

// MARK: - Row

struct SoundRow: View {
    @Environment(Store.self) private var store
    let sound: Sound
    let number: Int

    var body: some View {
        let playing = store.playback.soundID == sound.id
        HStack(spacing: 10) {
            Text("\(number)")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.tertiary)
                .frame(width: 26, alignment: .trailing)

            statusIcon(playing: playing)
                .frame(width: 18)

            VStack(alignment: .leading, spacing: 3) {
                Text(sound.name)
                    .fontWeight(playing ? .semibold : .regular)
                    .foregroundStyle(sound.isFailed ? AnyShapeStyle(.red) : AnyShapeStyle(.primary))
                    .lineLimit(1)
                if playing { RowProgress() }
            }

            Spacer(minLength: 8)

            if let label = sound.hotkeyLabel { Keycap(label) }

            Text(sound.duration.map(formatLength) ?? "")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: 52, alignment: .trailing)
        }
        .padding(.vertical, 3)
        .help(sound.error ?? sound.path)
        .animation(.smooth(duration: 0.25), value: playing)
    }

    @ViewBuilder private func statusIcon(playing: Bool) -> some View {
        if playing {
            Image(systemName: "speaker.wave.2.fill")
                .foregroundStyle(.tint)
                .symbolEffect(.variableColor.iterative, isActive: !store.playback.paused)
        } else if sound.isLoading {
            ProgressView().controlSize(.mini)
        } else if sound.isFailed {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        } else {
            Image(systemName: "waveform").foregroundStyle(.tertiary)
        }
    }
}

struct RowProgress: View {
    @Environment(Store.self) private var store
    var body: some View {
        ProgressView(value: store.playback.fraction)
            .progressViewStyle(.linear)
            .controlSize(.mini)
            .tint(.accentColor)
            .frame(maxWidth: 220)
    }
}

struct Keycap: View {
    let label: String
    init(_ label: String) { self.label = label }
    var body: some View {
        Text(label)
            .font(.system(.callout, design: .rounded).weight(.medium))
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .background(.quaternary.opacity(0.7), in: .rect(cornerRadius: 6))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
    }
}

// MARK: - Banner, toasts, drop overlay

struct SetupBanner: View {
    @Environment(Store.self) private var store

    var body: some View {
        let d = store.state.driver
        if store.state.settings.micDevice == nil {
            HStack(spacing: 12) {
                Image(systemName: "mic.badge.plus")
                    .font(.title2)
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Sounds only play to your speakers").font(.headline)
                    Text(d.installed
                         ? String(localized: "Route them into Discord, Zoom and games with Soundpad Virtual Mic.")
                         : String(localized: "Add Soundpad Virtual Mic so others hear them too. One-time setup, asks for your password."))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                if d.busy {
                    ProgressView().controlSize(.small)
                } else {
                    VStack(alignment: .trailing, spacing: 6) {
                        if d.installed {
                            Button("Use Virtual Mic") { store.send("use_virtual_mic") }
                                .buttonStyle(.glassProminent)
                        } else {
                            Button("Install") { store.send("install_driver") }
                                .buttonStyle(.glassProminent)
                                .disabled(!d.bundled)
                        }
                        // Already have BlackHole (or similar)? Offer it, but never take it silently.
                        if let other = store.state.otherLoopback {
                            Button("Use \(other.name) instead") {
                                store.send("set_mic_device", ["id": other.id])
                            }
                            .buttonStyle(.link)
                            .font(.caption)
                        }
                    }
                }
            }
            .padding(14)
            .glassEffect(.regular.tint(.orange.opacity(0.18)), in: .rect(cornerRadius: 20))
            .padding([.horizontal, .top], 12)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }
}

/// Older drivers had one two-way device; playing into it made macOS ask for
/// microphone permission. Offer the update (one password prompt).
struct DriverUpdateBanner: View {
    @Environment(Store.self) private var store

    var body: some View {
        let d = store.state.driver
        if d.installed && d.updateAvailable {
            HStack(spacing: 12) {
                Image(systemName: "arrow.down.circle.fill")
                    .font(.title2)
                    .foregroundStyle(.tint)
                Text("Update Soundpad Virtual Mic so Soundpad stops asking for microphone access.")
                    .font(.callout)
                Spacer(minLength: 8)
                if d.busy {
                    ProgressView().controlSize(.small)
                } else {
                    Button("Update") { store.send("install_driver") }
                        .buttonStyle(.glassProminent)
                }
            }
            .padding(12)
            .glassEffect(.regular.tint(.accentColor.opacity(0.15)), in: .rect(cornerRadius: 18))
            .padding([.horizontal, .top], 12)
        }
    }
}

/// Shown when "your" output is a loopback device (e.g. the Mac's sound output is
/// set to BlackHole for OBS): Soundpad's sounds then go there and you hear nothing.
struct SpeakerWarningBanner: View {
    @Environment(Store.self) private var store

    var body: some View {
        let st = store.state
        if st.speakerIsVirtual, let name = st.speakerDeviceName {
            HStack(spacing: 12) {
                Image(systemName: "speaker.slash.fill")
                    .font(.title2)
                    .foregroundStyle(.yellow)
                Text("Your Mac's sound output is “\(name)”, so you may not hear your sounds.")
                    .font(.callout)
                Spacer(minLength: 8)
                SettingsLink { Text("Choose Speakers…") }
                    .buttonStyle(.glass)
            }
            .padding(12)
            .glassEffect(.regular.tint(.yellow.opacity(0.15)), in: .rect(cornerRadius: 18))
            .padding([.horizontal, .top], 12)
        }
    }
}

struct ToastStack: View {
    @Environment(Store.self) private var store
    var body: some View {
        GlassEffectContainer(spacing: 8) {
            VStack(spacing: 8) {
                ForEach(store.toasts) { t in
                    Text(t.text)
                        .font(.callout)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 9)
                        .glassEffect(.regular, in: .capsule)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
        }
        .animation(.spring(duration: 0.35), value: store.toasts)
        .allowsHitTesting(false)
    }
}

struct DropOverlay: View {
    var playlist: String?
    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 24)
                .strokeBorder(.tint, style: StrokeStyle(lineWidth: 3, dash: [10, 8]))
                .padding(12)
            Label(playlist.map { String(localized: "Drop to add to “\($0)”") } ?? String(localized: "Drop to add sounds"), systemImage: "square.and.arrow.down")
                .font(.title2.weight(.semibold))
                .padding(.horizontal, 24)
                .padding(.vertical, 14)
                .glassEffect(.regular.tint(.accentColor.opacity(0.25)), in: .capsule)
        }
        .allowsHitTesting(false)
    }
}

struct RenameSheet: View {
    @Environment(Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let sound: Sound
    @State private var name = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Rename Sound").font(.headline)
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .onSubmit(save)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Rename", action: save)
                    .buttonStyle(.glassProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 340)
        .onAppear { name = sound.name }
    }

    private func save() {
        store.rename(sound.id, name)
        dismiss()
    }
}

// Menu command plumbing (File ▸ Add Sounds…)
struct AddSoundsActionKey: FocusedValueKey { typealias Value = () -> Void }
extension FocusedValues {
    var addSoundsAction: (() -> Void)? {
        get { self[AddSoundsActionKey.self] }
        set { self[AddSoundsActionKey.self] = newValue }
    }
}

import SwiftUI

struct SettingsView: View {
    @Environment(Store.self) private var store
    @State private var recording: HotkeyTarget?
    @State private var confirmBoost = false

    var body: some View {
        let st = store.state
        let s = st.settings
        Form {
            Section {
                Picker("Speakers", selection: binding(s.speakerDevice, "set_speaker_device")) {
                    Text("System default").tag(String?.none)
                    Divider()
                    ForEach(st.outputs.filter { !$0.isSoundpad }) { Text($0.name).tag(Optional($0.id)) }
                }
                Picker("Virtual microphone", selection: binding(s.micDevice, "set_mic_device")) {
                    Text("Off").tag(String?.none)
                    Divider()
                    ForEach(st.outputs) { d in
                        Label(d.name, systemImage: d.isSoundpad ? "star.fill" : d.isVirtual ? "cable.connector" : "speaker.wave.2")
                            .tag(Optional(d.id))
                    }
                }
                Picker("Mix in my microphone", selection: binding(s.inputDevice, "set_input_device")) {
                    Text("Off").tag(String?.none)
                    Text(st.defaultInputName.map { String(localized: "System default (\($0))") } ?? String(localized: "System default")).tag(Optional(""))
                    Divider()
                    ForEach(st.inputs.filter { !$0.isSoundpad && !$0.isVirtual }) { Text($0.name).tag(Optional($0.id)) }
                }
                .disabled(s.micDevice == nil)
                LabeledContent("My mic volume") {
                    Slider(value: Binding(get: { s.passthroughVolume }, set: {
                        store.send("set_passthrough_volume", ["value": $0])
                    }), in: 0...2)
                }
                .disabled(s.inputDevice == nil)
                VStack(alignment: .leading, spacing: 4) {
                    Toggle("Allow sound volume up to 150% for others", isOn: Binding(
                        get: { s.micBoost },
                        set: { on in
                            // Turning it on is risky for listeners: ask first.
                            if on { confirmBoost = true } else { store.send("set_mic_boost", ["on": false]) }
                        }))
                    Text("Lets the 🎙 volume go above 100%. Off by default.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .disabled(s.micDevice == nil)
                .alert("Allow volume up to 150%?", isPresented: $confirmBoost) {
                    Button("Turn On", role: .destructive) { store.send("set_mic_boost", ["on": true]) }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text("Above 100%, sounds can distort and be much louder for people in your call or stream. Turn the 🎙 volume down if others say it’s too loud.")
                }
            } header: {
                Text("Audio")
            } footer: {
                if st.driver.selected {
                    Text("In Discord, Zoom, Teams or games choose **Soundpad Virtual Mic** as your microphone.")
                } else if let a = s.speakerDevice, a == s.micDevice {
                    Label("Speakers and virtual microphone are the same device.", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.orange)
                }
            }

            Section("Playback") {
                Toggle("Overlap sounds", isOn: Binding(get: { s.overlap }, set: { store.send("set_overlap", ["on": $0]) }))
                Text("When off, a new sound replaces the one playing — like Soundpad.")
                    .font(.caption).foregroundStyle(.secondary)
                hotkeyRow("Stop all sounds", .stop, st.stopHotkeyLabel)
                hotkeyRow("Pause / resume", .pause, st.pauseHotkeyLabel)
            }

            Section("Soundpad Virtual Mic") {
                driverSection
            }

            Section {
                DisclosureGroup("Diagnostics") {
                    ForEach(st.diagnostics, id: \.self) { d in
                        LabeledContent(d.output == "speakers" ? String(localized: "Speakers") : String(localized: "Virtual mic")) {
                            Text(verbatim: "\(d.device) · \(d.sampleRate) Hz · \(d.bufferFrames) frames · DSP \(String(format: "%.1f", d.dspLoadPercent))% (peak \(String(format: "%.1f", d.dspPeakPercent))%)")
                                .font(.caption.monospacedDigit())
                                .multilineTextAlignment(.trailing)
                        }
                    }
                    if let input = st.inputDeviceName {
                        LabeledContent("Mic input", value: input)
                    }
                    LabeledContent("Library") {
                        Button("Show in Finder") { store.send("open_library_folder") }
                    }
                }
            }

            Section("Updates") {
                UpdatesSection()
            }

            Section {
                credits
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
        .frame(minHeight: 560)
        .onAppear { store.send("refresh_devices") }
        .sheet(item: $recording) { HotkeyRecorder(target: $0) }
    }

    private var credits: some View {
        let author = "bynarig"
        let coauthor = "Claude"
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        return VStack(spacing: 3) {
            Text("Created by \(author) and \(coauthor)")
                .font(.callout)
            Text(verbatim: "Soundpad \(version)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .multilineTextAlignment(.center)
        .padding(.vertical, 2)
    }

    @ViewBuilder private var driverSection: some View {
        let d = store.state.driver
        HStack(spacing: 12) {
            Image(systemName: d.installed ? "checkmark.seal.fill" : "mic.badge.plus")
                .font(.title2)
                .foregroundStyle(d.installed ? .green : .orange)
                .symbolRenderingMode(.hierarchical)
            VStack(alignment: .leading, spacing: 2) {
                Text(d.installed ? String(localized: "Installed") : String(localized: "Not installed")).font(.headline)
                Text("A virtual audio device that other apps use as a microphone. Installing asks for your password and restarts audio for a second.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if d.busy {
                ProgressView().controlSize(.small)
            } else if d.installed {
                VStack(alignment: .trailing) {
                    if d.updateAvailable {
                        Button("Update") { store.send("install_driver") }.buttonStyle(.glassProminent)
                    }
                    Menu("Manage") {
                        Button("Reinstall") { store.send("install_driver") }
                        Button("Uninstall", role: .destructive) { store.send("uninstall_driver") }
                    }
                    .fixedSize()
                }
            } else {
                Button("Install") { store.send("install_driver") }
                    .buttonStyle(.glassProminent)
                    .disabled(!d.bundled)
            }
        }
        Text("Based on the open-source BlackHole driver by Existential Audio (GPL-3.0).")
            .font(.caption2)
            .foregroundStyle(.tertiary)
    }

    private func hotkeyRow(_ title: LocalizedStringKey, _ target: HotkeyTarget, _ label: String?) -> some View {
        LabeledContent(title) {
            HStack {
                Button { recording = target } label: {
                    Text(label ?? String(localized: "Record…")).frame(minWidth: 70)
                }
                .buttonStyle(.glass)
                if label != nil {
                    Button { store.clearHotkey(target) } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Clear")
                }
            }
        }
    }

    private func binding(_ value: String?, _ cmd: String) -> Binding<String?> {
        Binding(get: { value }, set: { new in
            store.send(cmd, ["id": new.map { $0 as Any } ?? NSNull()])
        })
    }
}

/// Version, automatic checks and the update button.
struct UpdatesSection: View {
    @State private var updater = Updater.shared

    var body: some View {
        LabeledContent("Version") {
            Text(verbatim: updater.currentVersion).monospacedDigit()
        }
        VStack(alignment: .leading, spacing: 4) {
            Toggle("Check for updates automatically", isOn: $updater.automaticChecks)
            Text("Soundpad checks when it starts and downloads new versions in the background. You choose when to restart.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .disabled(!updater.isConfigured)
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            status
            Spacer(minLength: 8)
            action
        }
    }

    @ViewBuilder private var status: some View {
        switch updater.phase {
        case .idle:
            if !updater.isConfigured {
                Text("Updates aren't set up for this build.").foregroundStyle(.secondary)
            }
        case .checking:
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Checking…").foregroundStyle(.secondary)
            }
        case .upToDate:
            Label("Soundpad is up to date.", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .available(let info):
            VStack(alignment: .leading, spacing: 4) {
                Text("Version \(info.version) is available.").font(.headline)
                if let notes = info.notes, !notes.isEmpty {
                    Text(verbatim: notes).font(.caption).foregroundStyle(.secondary)
                }
            }
        case .downloading(let progress):
            VStack(alignment: .leading, spacing: 4) {
                Text("Downloading…").foregroundStyle(.secondary)
                ProgressView(value: progress).frame(maxWidth: 220)
            }
        case .ready(let info, _):
            VStack(alignment: .leading, spacing: 4) {
                Text("Version \(info.version) is ready to install.").font(.headline)
                if let notes = info.notes, !notes.isEmpty {
                    Text(verbatim: notes).font(.caption).foregroundStyle(.secondary)
                }
            }
        case .installing:
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Installing…").foregroundStyle(.secondary)
            }
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private var action: some View {
        if updater.readyUpdate != nil {
            Button("Restart to Update") { updater.installReady() }
                .buttonStyle(.glassProminent)
        } else if let info = updater.availableUpdate {
            Button("Install and Relaunch") { Task { await updater.install(info) } }
                .buttonStyle(.glassProminent)
        } else {
            Button("Check for Updates") { Task { await updater.check(userInitiated: true) } }
                .disabled({
                    switch updater.phase {
                    case .checking, .downloading, .ready, .installing: true
                    default: false
                    }
                }())
        }
    }
}

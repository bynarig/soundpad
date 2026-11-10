import SwiftUI

/// Floating Liquid Glass transport: play/stop, now playing, and the two output
/// controls. The pieces sit in one GlassEffectContainer so they blend and
/// morph together like system controls.
struct PlayerBar: View {
    @Environment(Store.self) private var store
    @Namespace private var ns

    var body: some View {
        GlassEffectContainer(spacing: 14) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) {
                    transport
                    outputs
                }
                // Narrow windows: volume controls sit above the transport, which stays at the bottom.
                VStack(alignment: .trailing, spacing: 10) {
                    outputs
                    transport
                }
            }
        }
    }

    private var transport: some View {
        let pb = store.playback
        let title = pb.isPlaying ? (store.sound(pb.soundID)?.name ?? String(localized: "Playing")) : String(localized: "Nothing playing")
        return HStack(spacing: 10) {
            HStack(spacing: 2) {
                IconButton("backward.end.fill", help: "Previous (⌥⌘[)") { store.previous() }
                Button {
                    if pb.isPlaying {
                        store.togglePause()
                    } else if let first = store.visibleSounds.first(where: \.isReady) {
                        store.play(first.id)
                    }
                } label: {
                    Image(systemName: pb.isPlaying && !pb.paused ? "pause.fill" : "play.fill")
                        .font(.title2)
                        .contentTransition(.symbolEffect(.replace))
                        .frame(width: 30, height: 30)
                }
                .buttonStyle(.glassProminent)
                .buttonBorderShape(.circle)
                .help(pb.isPlaying ? String(localized: "Pause / resume (Space)") : String(localized: "Play"))
                IconButton("forward.end.fill", help: "Next (⌥⌘])") { store.next() }
                IconButton("stop.fill", help: "Stop (Esc)", action: store.stop)
                    .disabled(!pb.isPlaying)
            }
            // Media transport keeps its direction in right-to-left languages (Apple HIG).
            .environment(\.layoutDirection, .leftToRight)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .center, spacing: 4) {
                    Text(title)
                        .font(.headline)
                        .foregroundStyle(pb.isPlaying ? .primary : .secondary)
                        .lineLimit(1)
                        .contentTransition(.opacity)
                    if pb.voices > 1 {
                        Text(verbatim: "+\(pb.voices - 1)").font(.caption.bold()).foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 6)
                    HStack(spacing: 4) {
                        IconButton("gobackward.10", small: true, help: "Back 10 seconds (⌘[)") { store.seek(by: -10) }
                            .disabled(!pb.isPlaying)
                        if pb.isPlaying { PlaybackTime() }
                        IconButton("goforward.10", small: true, help: "Forward 10 seconds (⌘])") { store.seek(by: 10) }
                            .disabled(!pb.isPlaying)
                    }
                    .environment(\.layoutDirection, .leftToRight)
                    ModeMenu()
                }
                ProgressTrack()
            }
            .frame(minWidth: 240)
        }
        .padding(.leading, 6)
        .padding(.trailing, 16)
        .padding(.vertical, 6)
        .glassEffect(.regular, in: .capsule)
        .glassEffectID("transport", in: ns)
        .animation(.smooth(duration: 0.3), value: pb.isPlaying)
    }

    private var outputs: some View {
        let s = store.state.settings
        return HStack(spacing: 14) {
            OutputControl(
                icon: "speaker.wave.2.fill", offIcon: "speaker.slash.fill", help: String(localized: "Speakers — what you hear"),
                enabled: s.speakersEnabled, volume: s.speakerVolume,
                onToggle: { store.setEnabled(.speakers, $0) }, onVolume: { store.setVolume(.speakers, $0) })
            OutputControl(
                icon: "mic.fill", offIcon: "mic.slash.fill", help: String(localized: "Microphone — what others hear"),
                enabled: s.micEnabled && s.micDevice != nil, volume: s.micVolume,
                onToggle: { store.setEnabled(.mic, $0) }, onVolume: { store.setVolume(.mic, $0) })
                .disabled(s.micDevice == nil)
            LevelMeter()
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .glassEffect(.regular, in: .capsule)
        .glassEffectID("outputs", in: ns)
    }
}

/// "0:47 / 1:44". Its own view so ticking seconds don't rebuild the glass bar.
struct PlaybackTime: View {
    @Environment(Store.self) private var store
    var body: some View {
        let pb = store.playback
        Text(verbatim: "\(formatTime(Double(pb.elapsed))) / \(formatTime(pb.duration))")
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
    }
}

/// Progress bar drawn in a single Canvas (one layer instead of a
/// GeometryReader with two shape views). Redraws only when the quantized
/// fraction changes.
/// Progress bar drawn in a single Canvas. Click or drag anywhere on it to
/// jump to that point; it grows and shows a knob while hovered.
struct ProgressTrack: View {
    @Environment(Store.self) private var store
    @State private var width: CGFloat = 1
    @State private var hovering = false
    /// Fraction being dragged to (shown instead of the playback position).
    @State private var scrub: Double?

    var body: some View {
        let pb = store.playback
        let active = pb.isPlaying && pb.duration > 0
        let fraction = scrub ?? pb.fraction
        let big = active && (hovering || scrub != nil)
        Canvas { ctx, size in
            let h: CGFloat = big ? 7 : 5
            let y = (size.height - h) / 2
            ctx.fill(Path(roundedRect: CGRect(x: 0, y: y, width: size.width, height: h), cornerRadius: h / 2),
                     with: .style(.primary.opacity(0.12)))
            let w = max(active ? h : 0, size.width * fraction)
            if w > 0 {
                ctx.fill(Path(roundedRect: CGRect(x: 0, y: y, width: w, height: h), cornerRadius: h / 2),
                         with: .style(.tint))
            }
            if big {
                let r: CGFloat = 6
                let x = min(max(size.width * fraction, r), size.width - r)
                let knob = CGRect(x: x - r, y: size.height / 2 - r, width: r * 2, height: r * 2)
                ctx.fill(Path(ellipseIn: knob), with: .color(.white))
                ctx.stroke(Path(ellipseIn: knob), with: .color(.black.opacity(0.15)), lineWidth: 0.5)
            }
        }
        .frame(height: 14)
        .contentShape(Rectangle())
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = max($0, 1) }
        .onHover { hovering = $0 }
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { g in
                    guard active else { return }
                    scrub = min(max(g.location.x / width, 0), 1)
                }
                .onEnded { _ in
                    guard active, let f = scrub else { return }
                    store.seek(to: f * pb.duration)
                    // Keep showing the target until the new position is reported.
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { scrub = nil }
                })
        .help(active ? String(localized: "Click or drag to jump") : "")
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue("\(formatTime(Double(pb.elapsed))) of \(formatTime(pb.duration))")
        .accessibilityAdjustableAction { dir in
            store.seek(by: dir == .increment ? 10 : -10)
        }
        // Playback progress always runs left to right (Apple HIG), also in RTL languages.
        .environment(\.layoutDirection, .leftToRight)
    }
}

/// Small borderless symbol button for the player bar (no extra glass layers).
struct IconButton: View {
    let symbol: String
    let small: Bool
    let help: LocalizedStringKey
    let action: () -> Void

    init(_ symbol: String, small: Bool = false, help: LocalizedStringKey, action: @escaping () -> Void) {
        self.symbol = symbol
        self.small = small
        self.help = help
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(small ? .callout : .body)
                .frame(width: small ? 22 : 28, height: small ? 22 : 28)
                .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .help(help)
    }
}

/// Picks what happens when a sound ends: stop, repeat it, or play the list.
struct ModeMenu: View {
    @Environment(Store.self) private var store
    var body: some View {
        let mode = store.state.settings.playbackMode
        Menu {
            Picker("When a Sound Ends", selection: Binding(get: { mode }, set: { store.setMode($0) })) {
                ForEach(PlaybackMode.allCases) { m in
                    Label(m.title, systemImage: m.icon).tag(m)
                }
            }
            .pickerStyle(.inline)
        } label: {
            Image(systemName: mode.icon)
                .foregroundStyle(mode == .once ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .frame(width: 26)
        .help(Text(verbatim: "\(mode.title): \(mode.detail)"))
    }
}

struct OutputControl: View {
    let icon: String
    let offIcon: String
    let help: String
    let enabled: Bool
    let volume: Double
    let onToggle: (Bool) -> Void
    let onVolume: (Double) -> Void
    @State private var local: Double?

    var body: some View {
        HStack(spacing: 6) {
            Button { onToggle(!enabled) } label: {
                Image(systemName: enabled ? icon : offIcon)
                    .frame(width: 18, height: 18)
                    .contentTransition(.symbolEffect(.replace))
                    .foregroundStyle(enabled ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
            }
            .buttonStyle(.plain)
            .help(enabled ? String(localized: "\(help) — click to mute") : String(localized: "\(help) — click to unmute"))

            Slider(
                value: Binding(get: { local ?? volume }, set: { local = $0; onVolume($0) }),
                in: 0...1,
                onEditingChanged: { if !$0 { local = nil } }
            )
            .controlSize(.small)
            .frame(width: 84)
            .disabled(!enabled)
            .help(Text(verbatim: "\(help): \(Int((local ?? volume) * 100))%"))
        }
    }
}

/// 5-segment output meter drawn in one Canvas. It only redraws when the
/// number of lit segments changes (no per-frame animation).
struct LevelMeter: View {
    @Environment(Store.self) private var store
    var body: some View {
        let level = store.playback.level
        Canvas { ctx, size in
            let n = PlaybackModel.meterSegments
            let gap: CGFloat = 2
            let h = (size.height - gap * CGFloat(n - 1)) / CGFloat(n)
            for i in 0..<n {
                let y = size.height - CGFloat(i + 1) * h - CGFloat(i) * gap
                let color: Color = i >= level ? .primary.opacity(0.12) : i == n - 1 ? .red : i == n - 2 ? .yellow : .green
                ctx.fill(Path(roundedRect: CGRect(x: 0, y: y, width: size.width, height: h), cornerRadius: h / 2),
                         with: .color(color))
            }
        }
        .frame(width: 4, height: 23)
        .accessibilityLabel("Output level")
        .help("Output level")
    }
}

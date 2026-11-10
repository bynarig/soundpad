import SwiftUI

/// Floating Liquid Glass player: transport, now playing and the two volume
/// buttons on one piece of glass (related controls share one surface; no
/// glass stacked on glass). Volume sliders open in popovers from their
/// buttons, like Control Center's Sound module.
struct PlayerBar: View {
    @Environment(Store.self) private var store
    @Namespace private var ns

    var body: some View {
        GlassEffectContainer {
            transport
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
            .frame(minWidth: 180)

            outputs
        }
        .padding(.leading, 6)
        .padding(.trailing, 10)
        .padding(.vertical, 6)
        .glassEffect(.regular, in: .capsule)
        .glassEffectID("transport", in: ns)
        .animation(.smooth(duration: 0.3), value: pb.isPlaying)
    }

    private var outputs: some View {
        let s = store.state.settings
        return HStack(spacing: 2) {
            VolumeButton(
                kind: .speakers, help: String(localized: "Speakers — what you hear"),
                symbol: "speaker.wave.3.fill", mutedSymbol: "speaker.slash.fill",
                enabled: s.speakersEnabled, volume: s.speakerVolume)
            VolumeButton(
                kind: .mic, help: String(localized: "Microphone — what others hear"),
                symbol: "mic.fill", mutedSymbol: "mic.slash.fill",
                enabled: s.micEnabled && s.micDevice != nil, volume: s.micVolume,
                maxVolume: s.micBoost ? 1.5 : 1)
                .disabled(s.micDevice == nil)
        }
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

/// A volume button in the player bar. Click opens a popover with the slider
/// and a Mute switch; ⌥-click mutes or unmutes straight away. The symbol shows
/// the volume (speaker waves) and whether that side is muted.
struct VolumeButton: View {
    @Environment(Store.self) private var store
    let kind: OutputKind
    let help: String
    let symbol: String
    let mutedSymbol: String
    let enabled: Bool
    let volume: Double
    /// 1 = 100%; the mic can go to 1.5 with the boost setting.
    var maxVolume: Double = 1
    @State private var open = false
    @State private var local: Double?

    var body: some View {
        let v = local ?? volume
        Button {
            if NSEvent.modifierFlags.contains(.option) {
                store.setEnabled(kind, !enabled)
            } else {
                open.toggle()
            }
        } label: {
            Image(systemName: enabled ? symbol : mutedSymbol, variableValue: kind == .speakers ? v : nil)
                .contentTransition(.symbolEffect(.replace))
                .foregroundStyle(enabled ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                .frame(width: 28, height: 28)
                .contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .help(enabled ? String(localized: "\(help) — ⌥-click to mute") : String(localized: "\(help) — ⌥-click to unmute"))
        .accessibilityLabel(help)
        .accessibilityValue(enabled ? Text(v, format: .percent.precision(.fractionLength(0))) : Text("Mute"))
        .popover(isPresented: $open, arrowEdge: .top) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text(help).font(.headline)
                    Spacer(minLength: 12)
                    Toggle("Mute", isOn: Binding(get: { !enabled }, set: { store.setEnabled(kind, !$0) }))
                        .toggleStyle(.switch)
                        .controlSize(.small)
                }
                HStack(spacing: 8) {
                    Image(systemName: enabled ? symbol : mutedSymbol, variableValue: kind == .speakers ? 0 : nil)
                        .foregroundStyle(.secondary)
                        .frame(width: 18)
                    Slider(
                        value: Binding(get: { local ?? volume }, set: { local = $0; store.setVolume(kind, $0) }),
                        in: 0...maxVolume,
                        onEditingChanged: { if !$0 { local = nil } }
                    )
                    .disabled(!enabled)
                    .tint(v > 1 ? .orange : nil)
                    Text(v, format: .percent.precision(.fractionLength(0)))
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(v > 1 ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                        .frame(width: 44, alignment: .trailing)
                }
                LevelMeter(horizontal: true)
            }
            .padding(16)
            .frame(width: 300)
        }
    }
}

/// Output level meter: one continuous bar like a hardware meter. Colors sit
/// at fixed positions (green, yellow near the top, red at the end), the bar
/// rises instantly and falls smoothly, and a thin mark holds the recent peak.
struct LevelMeter: View {
    @Environment(Store.self) private var store
    var horizontal = false

    var body: some View {
        let level = store.playback.level
        let hold = store.playback.peakHold
        Canvas { ctx, size in
            let length = horizontal ? size.width : size.height
            let thick = horizontal ? size.height : size.width
            func rect(_ from: Double, _ to: Double) -> CGRect {
                let a = length * from, b = length * to
                return horizontal
                    ? CGRect(x: a, y: 0, width: b - a, height: thick)
                    : CGRect(x: 0, y: length - b, width: thick, height: b - a)
            }
            let track = Path(roundedRect: rect(0, 1), cornerRadius: thick / 2)
            ctx.fill(track, with: .color(.primary.opacity(0.12)))
            // Fixed color scale; the level reveals it, like a real meter.
            let gradient = Gradient(stops: [
                .init(color: .green, location: 0),
                .init(color: .green, location: 0.7),
                .init(color: .yellow, location: 0.85),
                .init(color: .red, location: 0.97),
            ])
            let shading = GraphicsContext.Shading.linearGradient(
                gradient,
                startPoint: horizontal ? CGPoint(x: 0, y: 0) : CGPoint(x: 0, y: size.height),
                endPoint: horizontal ? CGPoint(x: size.width, y: 0) : CGPoint(x: 0, y: 0))
            if level > 0 {
                ctx.drawLayer { layer in
                    layer.clip(to: track)
                    layer.fill(Path(rect(0, level)), with: shading)
                }
            }
            if hold > 0.01 {
                let markWidth = max(2, thick * 0.5) / length
                let at = min(max(hold - markWidth, 0), 1 - markWidth)
                ctx.drawLayer { layer in
                    layer.clip(to: track)
                    layer.fill(Path(rect(at, at + markWidth)), with: shading)
                }
            }
        }
        .frame(width: horizontal ? nil : 4, height: horizontal ? 5 : 23)
        .accessibilityLabel("Output level")
        .help("Output level")
    }
}

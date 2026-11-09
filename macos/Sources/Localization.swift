import Foundation

/// Messages from the Rust core arrive in English. Fixed ones are looked up as
/// localization keys directly; ones with variable parts are matched against
/// these English templates (`%@` = variable part) and re-formatted with the
/// translated template. Unknown messages are shown as they are.
private let coreTemplates: [String] = [
    "Skipped %@ unsupported file(s)",
    "Can't play “%@”: %@",
    "Hotkey %@ failed: %@",
    "Virtual mic setup failed: %@",
    "Could not save library: %@",
    "Global hotkeys unavailable: %@",
    "Output device not found: %@",
    "Input device not found: %@",
    "%@ (already used by another app?)",
]

/// Fixed core messages (listed so the localization tooling sees the keys).
let coreFixedMessages: [String.LocalizationValue] = [
    "Nothing to play",
    "Name can't be empty",
    "Still loading…",
    "Soundpad Virtual Mic is not available yet",
    "Soundpad Virtual Mic is ready — choose it as the microphone in your chat app",
    "Driver installed, but the device didn't appear. Try restarting your Mac.",
    "Soundpad Virtual Mic removed",
    "Add a modifier (⌃ ⌥ ⇧ ⌘) — a bare key would block typing everywhere",
]

private let compiledTemplates: [(String, NSRegularExpression)] = coreTemplates.compactMap { t in
    let parts = t.components(separatedBy: "%@").map(NSRegularExpression.escapedPattern(for:))
    let pattern = "^" + parts.joined(separator: "(.+?)") + "$"
    return (try? NSRegularExpression(pattern: pattern, options: [.dotMatchesLineSeparators])).map { (t, $0) }
}

func localizeCoreMessage(_ msg: String) -> String {
    let direct = Bundle.main.localizedString(forKey: msg, value: nil, table: nil)
    if direct != msg { return direct }
    let range = NSRange(msg.startIndex..., in: msg)
    for (template, re) in compiledTemplates {
        guard let m = re.firstMatch(in: msg, range: range) else { continue }
        let args: [String] = (1..<m.numberOfRanges).compactMap { i in
            Range(m.range(at: i), in: msg).map { localizeCoreMessage(String(msg[$0])) }
        }
        let format = Bundle.main.localizedString(forKey: template, value: nil, table: nil)
        return String(format: format, arguments: args.map { $0 as CVarArg })
    }
    return msg
}

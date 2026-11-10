import Foundation

// Unit tests for the Swift front end. Run with `scripts/test_swift.sh`: it
// compiles these together with the app's sources (without the app's entry
// point) and the Rust core, then runs them. A tiny runner instead of XCTest,
// because the app is built with swiftc rather than an Xcode project.

@MainActor
var failures = 0
@MainActor
var checks = 0

@MainActor
func expect(_ condition: @autoclosure () -> Bool, _ message: String, file: StaticString = #fileID, line: UInt = #line) {
    checks += 1
    if !condition() {
        failures += 1
        print("  FAIL \(file):\(line): \(message)")
    }
}

@MainActor
func test(_ name: String, _ body: () throws -> Void) {
    let before = failures
    do { try body() } catch {
        failures += 1
        print("  FAIL: threw \(error)")
    }
    print(failures == before ? "ok   \(name)" : "FAIL \(name)")
}

/// The core sends JSON with snake_case keys; the app decodes it like this.
func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    let d = JSONDecoder()
    d.keyDecodingStrategy = .convertFromSnakeCase
    return try d.decode(T.self, from: Data(json.utf8))
}

func sound(_ id: UInt64, _ name: String, duration: Double? = nil, added: Double? = nil, status: String = "ready") -> Sound {
    Sound(id: id, name: name, path: "/sounds/\(name).mp3", hotkey: nil, hotkeyLabel: nil, status: status,
          duration: duration, error: nil, added: added)
}

@main
enum SwiftTests {
    @MainActor
    static func main() {
        test("version comparison is numeric") {
            expect(Updater.compare("0.10.0", "0.9.9") == .orderedDescending, "0.10.0 > 0.9.9")
            expect(Updater.compare("0.3.2", "0.3.2") == .orderedSame, "equal versions")
            expect(Updater.compare("0.3", "0.3.0") == .orderedSame, "missing part counts as 0")
            expect(Updater.compare("1.0.0", "1.0.1") == .orderedAscending, "1.0.0 < 1.0.1")
        }

        test("update feed decodes") {
            let info = try JSONDecoder().decode(UpdateInfo.self, from: Data("""
                {"version":"0.3.2","url":"https://example.com/Soundpad-0.3.2.zip","signature":"abc",
                 "notes":"Notes","minimumSystemVersion":"26.0"}
                """.utf8))
            expect(info.version == "0.3.2", "version")
            expect(info.url.lastPathComponent == "Soundpad-0.3.2.zip", "url")
            expect(info.minimumSystemVersion == "26.0", "minimum system")
        }

        test("sounds decode, including the missing state") {
            let s = try decode(Sound.self, """
                {"id":7,"name":"Airhorn","path":"/x/Airhorn.mp3","hotkey":"alt+Digit1","hotkey_label":"⌥1",
                 "added":1700000000,"status":"ready","duration":3.5}
                """)
            expect(s.isReady && !s.isMissing, "ready")
            expect(s.hotkeyLabel == "⌥1", "hotkey label")
            expect(s.addedDate == Date(timeIntervalSince1970: 1_700_000_000), "date added")
            expect(s.tooltip == "/x/Airhorn.mp3", "tooltip is the path")

            let m = try decode(Sound.self, #"{"id":8,"name":"Gone","path":"/x/Gone.mp3","status":"missing"}"#)
            expect(m.isMissing && !m.isReady, "missing")
            expect(m.addedDate == nil && m.duration == nil, "no date or length")
            expect(m.tooltip.contains("/x/Gone.mp3"), "tooltip names the old path")

            let f = try decode(Sound.self, #"{"id":9,"name":"Bad","path":"/x/Bad.mp3","status":"failed","error":"nope"}"#)
            expect(f.isFailed && f.tooltip == "nope", "failed shows the error")
        }

        test("settings decode with the mic boost flag") {
            let s = try decode(CoreSettings.self, """
                {"speaker_device":null,"mic_device":"vm","input_device":null,"speakers_enabled":true,
                 "mic_enabled":false,"speaker_volume":0.8,"mic_volume":1.25,"passthrough_volume":1,
                 "mic_boost":true,"overlap":false,"stop_hotkey":null,"pause_hotkey":null,"playback_mode":"loop"}
                """)
            expect(s.micBoost, "mic boost on")
            expect(s.micVolume == 1.25, "mic volume above 100%")
            expect(!s.micEnabled, "mic muted")
            expect(s.playbackMode == .loop, "playback mode")
        }

        let sounds = [
            sound(1, "bravo", duration: 30, added: 300),
            sound(2, "Alpha", duration: 5, added: 100),
            sound(3, "charlie", duration: nil, added: nil, status: "loading"),
            sound(4, "delta", duration: 70, added: 200),
        ]

        test("manual order keeps the library order") {
            expect(SoundSort.manual.apply(sounds, reversed: false).map(\.id) == [1, 2, 3, 4], "as is")
            expect(SoundSort.manual.apply(sounds, reversed: true).map(\.id) == [4, 3, 2, 1], "reversed")
        }

        test("sort by name ignores case") {
            expect(SoundSort.name.apply(sounds, reversed: false).map(\.id) == [2, 1, 3, 4], "A–Z")
            expect(SoundSort.name.apply(sounds, reversed: true).map(\.id) == [4, 3, 1, 2], "Z–A")
        }

        test("sort by length puts unknown lengths last") {
            expect(SoundSort.length.apply(sounds, reversed: false).map(\.id) == [2, 1, 4, 3], "shortest first")
            expect(SoundSort.length.apply(sounds, reversed: true).map(\.id) == [4, 1, 2, 3], "longest first, unknown last")
        }

        test("sort by date added puts undated sounds last") {
            expect(SoundSort.added.apply(sounds, reversed: false).map(\.id) == [2, 4, 1, 3], "oldest first")
            expect(SoundSort.added.apply(sounds, reversed: true).map(\.id) == [1, 4, 2, 3], "newest first")
        }

        test("sorting is stable for equal values") {
            let same = [sound(1, "a", duration: 3), sound(2, "b", duration: 3), sound(3, "c", duration: 3)]
            expect(SoundSort.length.apply(same, reversed: false).map(\.id) == [1, 2, 3], "keeps order")
            expect(SoundSort.length.apply(same, reversed: true).map(\.id) == [1, 2, 3], "keeps order reversed")
        }

        test("length filter") {
            expect(LengthFilter.any.matches(nil), "any includes unknown")
            expect(LengthFilter.short.matches(9.9) && !LengthFilter.short.matches(10), "under 10 s")
            expect(LengthFilter.medium.matches(10) && LengthFilter.medium.matches(60), "10 s to 1 min, inclusive")
            expect(!LengthFilter.medium.matches(60.1) && LengthFilter.long.matches(60.1), "over 1 min")
            expect(!LengthFilter.long.matches(nil), "unknown length is filtered out")
        }

        test("date added filter") {
            let now = Date()
            let day: TimeInterval = 86_400
            expect(AddedFilter.any.matches(nil, now: now), "any includes undated")
            expect(AddedFilter.today.matches(now, now: now), "today")
            expect(!AddedFilter.today.matches(now - 2 * day, now: now), "two days ago is not today")
            expect(AddedFilter.week.matches(now - 6 * day, now: now), "6 days ago is in the past week")
            expect(!AddedFilter.week.matches(now - 8 * day, now: now), "8 days ago is not")
            expect(AddedFilter.month.matches(now - 29 * day, now: now), "29 days ago is in the past month")
            expect(!AddedFilter.month.matches(nil, now: now), "undated is filtered out")
        }

        test("core messages pass through or fill their template") {
            expect(localizeCoreMessage("Something new") == "Something new", "unknown message unchanged")
            expect(localizeCoreMessage("2 sound file(s) are missing") == "2 sound file(s) are missing", "template round-trip")
            expect(localizeCoreMessage("“Airhorn” is missing. Was it moved or deleted?")
                == "“Airhorn” is missing. Was it moved or deleted?", "quoted template round-trip")
        }

        print("\n\(checks) checks, \(failures) failed")
        exit(failures == 0 ? 0 : 1)
    }
}

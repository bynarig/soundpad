// Ed25519 keys and signatures for Soundpad updates (Apple CryptoKit, no dependencies).
//   swift scripts/update_tool.swift generate-keys <private-key-file>   → prints the public key
//   swift scripts/update_tool.swift public-key    <private-key-file>   → prints the public key
//   swift scripts/update_tool.swift sign          <private-key-file> <file> → prints the signature
//   swift scripts/update_tool.swift verify        <public-key> <file> <signature>
import CryptoKit
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data((msg + "\n").utf8))
    exit(1)
}

func loadPrivate(_ path: String) -> Curve25519.Signing.PrivateKey {
    guard let text = try? String(contentsOfFile: path, encoding: .utf8),
          let raw = Data(base64Encoded: text.trimmingCharacters(in: .whitespacesAndNewlines)),
          let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: raw)
    else { fail("Cannot read private key at \(path)") }
    return key
}

let args = CommandLine.arguments
switch (args.count > 1 ? args[1] : "", args.count) {
case ("generate-keys", 3):
    let path = args[2]
    if FileManager.default.fileExists(atPath: path) { fail("\(path) already exists — refusing to overwrite your signing key") }
    let key = Curve25519.Signing.PrivateKey()
    let dir = (path as NSString).deletingLastPathComponent
    try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    guard FileManager.default.createFile(atPath: path, contents: Data(key.rawRepresentation.base64EncodedString().utf8),
                                         attributes: [.posixPermissions: 0o600])
    else { fail("Cannot write \(path)") }
    print(key.publicKey.rawRepresentation.base64EncodedString())
case ("public-key", 3):
    print(loadPrivate(args[2]).publicKey.rawRepresentation.base64EncodedString())
case ("sign", 4):
    guard let data = FileManager.default.contents(atPath: args[3]) else { fail("Cannot read \(args[3])") }
    print(try! loadPrivate(args[2]).signature(for: data).base64EncodedString())
case ("verify", 5):
    guard let raw = Data(base64Encoded: args[2]), let key = try? Curve25519.Signing.PublicKey(rawRepresentation: raw),
          let data = FileManager.default.contents(atPath: args[3]), let sig = Data(base64Encoded: args[4])
    else { fail("bad arguments") }
    if key.isValidSignature(sig, for: data) { print("OK") } else { fail("INVALID signature") }
default:
    fail("usage: update_tool.swift generate-keys|public-key <key> | sign <key> <file> | verify <pubkey> <file> <sig>")
}

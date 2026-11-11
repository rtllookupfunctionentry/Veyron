import CryptoKit
import Foundation
import Network
import Security

/// The remote desktop over a network (never over USB, which stays on the cable): sealed, and only for a
/// computer the phone has been paired with.
///
/// The keys come from the Noise handshake Noise_XX_25519_ChaChaPoly_SHA256, with the phone answering. Apple
/// ships no Noise, so it is written out here from the Noise specification (revision 34, sections 5 and 7.5);
/// Veyron Remote does its half with the `snow` library, and remote/tests/fake-phone.py has this half in Python,
/// which is tried against that library. C is the computer, S the phone; e is a fresh key, s the lasting one:
///
///     C -> S   "VYN2", then  e                     "VYN2" is also the handshake's prologue
///     S -> C   e, ee, s, es                        96 bytes
///     C -> S   s, se                               64 bytes
///
/// After it both ways are Noise's transport messages, each behind its length ([UInt16 BE], at most 60 016):
/// ChaCha20-Poly1305 with the key for that direction and the count of messages sent that way as the nonce.
/// What they carry is the remote desktop's own stream, after this:
///
///     C -> S   [flags][name length][name][SHA256(rC)]      flags bit 0: the computer has this phone's key;
///                                                          rC is 32 random bytes it keeps to itself for now
///     S -> C   0xA1                                         come in (both know each other's lasting key)
///         or   0xA2, rS                                     pair first: 32 random bytes of the phone's
///                                                          (only while the phone's user has pairing open)
///     C -> S   rC                                           which must hash to what was promised
///     S -> C   0xA1                                         once the phone's user has agreed
///
/// The handshake proves to each side that the other holds its lasting key. Whether that key is the right
/// computer's is what pairing settles: both show six digits made from the handshake's hash, rC and rS, and
/// the user compares them. Someone in the middle holds two different handshakes; he must give the phone his
/// promise before seeing rS, and the computer his own random bytes before seeing rC, so he cannot steer either
/// number, and the two agree by a one in a million chance.
final class RemoteSeal {
    static let go: UInt8 = 0xA1
    static let pair: UInt8 = 0xA2
    private static let magic = Data("VYN2".utf8)
    private static let chunk = 60_000

    /// The computer's lasting public key, and the handshake's hash, which no two connections share.
    let computer: Data
    let binding: Data
    private let sendKey: SymmetricKey
    private let openKey: SymmetricKey
    private let lock = NSLock()
    private var sent: UInt64 = 0
    private var opened: UInt64 = 0
    private var waiting = [UInt8]() // received, not yet a whole message

    private init(sendKey: SymmetricKey, openKey: SymmetricKey, computer: Data, binding: Data) {
        self.sendKey = sendKey
        self.openKey = openKey
        self.computer = computer
        self.binding = binding
    }

    /// Noise's nonce for ChaCha20-Poly1305: four zero bytes, then the count, little-endian.
    private static func nonce(_ count: UInt64) -> ChaChaPoly.Nonce {
        var bytes = [UInt8](repeating: 0, count: 4)
        withUnsafeBytes(of: count.littleEndian) { bytes.append(contentsOf: $0) }
        return try! ChaChaPoly.Nonce(data: bytes) // twelve bytes, which is what it takes
    }

    /// Seals and hands over in one step, so messages leave in the order they were counted whichever queue sends.
    func send(_ c: NWConnection, _ data: Data, completion: NWConnection.SendCompletion = .idempotent) {
        lock.lock()
        defer { lock.unlock() }
        var out = Data(capacity: data.count + 18 * (data.count / Self.chunk + 1))
        var at = data.startIndex
        while at < data.endIndex {
            let end = min(at + Self.chunk, data.endIndex)
            guard let box = try? ChaChaPoly.seal(data[at..<end], using: sendKey, nonce: Self.nonce(sent)) else { c.cancel(); return }
            sent += 1
            withUnsafeBytes(of: UInt16(box.ciphertext.count + box.tag.count).bigEndian) { out.append(contentsOf: $0) }
            out.append(box.ciphertext)
            out.append(box.tag)
            at = end
        }
        c.send(content: out, completion: completion)
    }

    /// What the bytes just received say, once whole messages of them are there. nil: they were not sealed by
    /// the computer this connection began with, and the connection is to be dropped.
    func open(_ data: Data) -> Data? {
        waiting.append(contentsOf: data)
        var plain = Data()
        var at = 0
        while waiting.count - at >= 2 {
            let n = Int(waiting[at]) << 8 | Int(waiting[at + 1])
            guard n >= 16, n <= Self.chunk + 16 else { return nil }
            guard waiting.count - at >= 2 + n else { break }
            let body = waiting[(at + 2)..<(at + 2 + n)]
            guard let box = try? ChaChaPoly.SealedBox(nonce: Self.nonce(opened), ciphertext: body.dropLast(16), tag: body.suffix(16)),
                  let piece = try? ChaChaPoly.open(box, using: openKey) else { return nil }
            opened += 1
            plain.append(piece)
            at += 2 + n
        }
        waiting.removeFirst(at)
        return plain
    }

    /// The next `count` bytes the computer sends, opened, and whatever came behind them. nil when the
    /// connection ended or was tampered with first.
    func take(_ c: NWConnection, _ count: Int, have: Data = Data(), done: @escaping ((Data, Data)?) -> Void) {
        if have.count >= count {
            done((Data(have.prefix(count)), Data(have.dropFirst(count))))
            return
        }
        c.receive(minimumIncompleteLength: 1, maximumLength: 4096) { data, _, _, _ in
            guard let data, !data.isEmpty, let plain = self.open(data) else { done(nil); return }
            self.take(c, count, have: Data(have + plain), done: done)
        }
    }

    /// The computer's first words: whether it knows this phone, its name, its promise for a pairing, and
    /// whatever of the stream came along behind them.
    func greeting(_ c: NWConnection, done: @escaping ((knows: Bool, name: String, pledge: Data, rest: Data)?) -> Void) {
        take(c, 2) { head in
            guard let (head, rest) = head else { done(nil); return }
            let (flags, length) = (head[0], Int(head[1]))
            self.take(c, length + 32, have: rest) { body in
                guard let (body, rest) = body else { done(nil); return }
                let raw = String(decoding: body.prefix(length), as: UTF8.self)
                let name = String(raw.filter { !$0.isNewline && ($0.asciiValue ?? 32) >= 32 }.prefix(63)) // no control characters
                done((flags & 1 != 0, name.isEmpty ? "A computer" : name, Data(body.suffix(32)), rest))
            }
        }
    }

    /// 32 random bytes, for the phone's part of a pairing's number.
    static func random() -> Data {
        SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
    }

    /// The six digits both screens show when pairing. `mine` and `theirs` are rS and rC above; nil when
    /// the computer's are not the ones it promised.
    func number(mine: Data, theirs: Data, pledge: Data) -> String? {
        guard Data(SHA256.hash(data: theirs)) == pledge else { return nil }
        let hash = Data(SHA256.hash(data: Data("veyron pairing".utf8) + binding + theirs + mine))
        return String(format: "%06d", hash.prefix(4).reduce(UInt32(0)) { $0 << 8 | UInt32($1) } % 1_000_000)
    }

    // MARK: - The handshake

    /// Noise's handshake state for this one pattern: the running hash h, the chaining key ck, and the key k
    /// that messages are sealed with once there is one (sections 5.1 and 5.2).
    private final class Noise {
        var h = Data("Noise_XX_25519_ChaChaPoly_SHA256".utf8) // 32 bytes, so it is the first hash as it stands
        var ck = Data("Noise_XX_25519_ChaChaPoly_SHA256".utf8)
        var k: SymmetricKey?
        var n: UInt64 = 0

        func mixHash(_ data: Data) {
            h = Data(SHA256.hash(data: h + data))
        }

        /// Noise's HKDF with two outputs (section 4.3).
        static func hkdf(_ ck: Data, _ material: Data) -> (Data, Data) {
            let temp = SymmetricKey(data: Data(HMAC<SHA256>.authenticationCode(for: material, using: SymmetricKey(data: ck))))
            let one = Data(HMAC<SHA256>.authenticationCode(for: Data([1]), using: temp))
            return (one, Data(HMAC<SHA256>.authenticationCode(for: one + Data([2]), using: temp)))
        }

        func mixKey(_ material: Data) {
            let (chain, key) = Self.hkdf(ck, material)
            (ck, k, n) = (chain, SymmetricKey(data: key), 0)
        }

        /// EncryptAndHash: sealed with h as the associated data once there is a key; mixed into h either way.
        func seal(_ plain: Data) -> Data? {
            var out = plain
            if let k {
                guard let box = try? ChaChaPoly.seal(plain, using: k, nonce: RemoteSeal.nonce(n), authenticating: h) else { return nil }
                out = box.ciphertext + box.tag
                n += 1
            }
            mixHash(out)
            return out
        }

        /// DecryptAndHash.
        func open(_ sealed: Data) -> Data? {
            var out = sealed
            if let k {
                guard sealed.count >= 16,
                      let box = try? ChaChaPoly.SealedBox(nonce: RemoteSeal.nonce(n), ciphertext: sealed.dropLast(16), tag: sealed.suffix(16)),
                      let plain = try? ChaChaPoly.open(box, using: k, authenticating: h) else { return nil }
                out = plain
                n += 1
            }
            mixHash(sealed)
            return out
        }
    }

    private static func agree(_ mine: Curve25519.KeyAgreement.PrivateKey, _ theirs: Data) -> Data? {
        guard let key = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: theirs),
              let secret = try? mine.sharedSecretFromKeyAgreement(with: key) else { return nil }
        return secret.withUnsafeBytes { Data($0) }
    }

    /// The phone's part of the handshake. `done` gets nil when the other end is not a Veyron Remote that
    /// speaks this, or a message of it does not open.
    static func accept(_ c: NWConnection, done: @escaping (RemoteSeal?) -> Void) {
        c.receive(minimumIncompleteLength: 36, maximumLength: 36) { first, _, _, _ in
            guard let first, first.count == 36, first.prefix(4) == magic else { done(nil); return }
            let noise = Noise()
            noise.mixHash(magic) // the prologue
            // -> e
            let theirFresh = Data(first.suffix(32))
            noise.mixHash(theirFresh)
            guard noise.open(Data()) != nil else { done(nil); return } // the empty payload, which is hashed all the same
            // <- e, ee, s, es
            let fresh = Curve25519.KeyAgreement.PrivateKey()
            var second = fresh.publicKey.rawRepresentation
            noise.mixHash(second)
            guard let ee = agree(fresh, theirFresh) else { done(nil); return }
            noise.mixKey(ee)
            guard let lasting = noise.seal(identity.publicKey.rawRepresentation), let es = agree(identity, theirFresh) else { done(nil); return }
            second.append(lasting)
            noise.mixKey(es)
            guard let empty = noise.seal(Data()) else { done(nil); return }
            second.append(empty)
            c.send(content: second, completion: .idempotent)
            // -> s, se
            c.receive(minimumIncompleteLength: 64, maximumLength: 64) { third, _, _, _ in
                guard let third, third.count == 64,
                      let computer = noise.open(Data(third.prefix(48))), computer.count == 32,
                      let se = agree(fresh, computer) else { done(nil); return }
                noise.mixKey(se)
                guard noise.open(Data(third.suffix(16))) != nil else { done(nil); return }
                // Split: the first key is for what the one who began (the computer) sends, the second for the phone's.
                let (toPhone, toComputer) = Noise.hkdf(noise.ck, Data())
                done(RemoteSeal(sendKey: SymmetricKey(data: toComputer), openKey: SymmetricKey(data: toPhone), computer: computer, binding: noise.h))
            }
        }
    }

    // MARK: - Who this phone is, and whom it knows

    /// The phone's lasting key, kept in the keychain of this device only.
    static let identity: Curve25519.KeyAgreement.PrivateKey = {
        let item: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
                                   kSecAttrService as String: "henge.remote.identity", kSecAttrAccount as String: "x25519"]
        var found: AnyObject?
        if SecItemCopyMatching(item.merging([kSecReturnData as String: true]) { $1 } as CFDictionary, &found) == errSecSuccess,
           let data = found as? Data, let key = try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: data) {
            return key
        }
        let key = Curve25519.KeyAgreement.PrivateKey()
        let status = SecItemAdd(item.merging([kSecValueData as String: key.rawRepresentation,
                                              kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]) { $1 } as CFDictionary, nil)
        if status != errSecSuccess {
            HengeLog.log("remote", "this phone's key could not be kept (\(status)): computers will have to pair again after Veyron restarts")
        }
        return key
    }()

    private static let pairedKey = "henge.remote.paired"

    /// The computers paired with, as their keys (base64) and names.
    static var paired: [[String: String]] {
        UserDefaults.standard.array(forKey: pairedKey) as? [[String: String]] ?? []
    }

    static func knows(_ computer: Data) -> Bool {
        paired.contains { $0["key"] == computer.base64EncodedString() }
    }

    static func remember(_ computer: Data, name: String) {
        let key = computer.base64EncodedString()
        UserDefaults.standard.set(paired.filter { $0["key"] != key } + [["key": key, "name": name]], forKey: pairedKey)
    }

    static func forgetAll() {
        UserDefaults.standard.removeObject(forKey: pairedKey)
    }
}

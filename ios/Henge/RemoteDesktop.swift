import Foundation
import Network
import VideoToolbox
import CoreMedia
import CoreVideo
import QuartzCore
import UIKit

/// Remote desktop: the Mac's screen through the phone's hardware encoder to one client on port 5901, and the
/// client's mouse and keyboard back as 6-byte messages:
/// [type][a][x: Int16 LE][y: Int16 LE], type 1 move (x, y), 2 button (a, x = down), 3 scroll (x, y), 4 key (a = HID usage, x = down),
/// 5 the computer's name (a = its length in bytes, which follow the message; shown instead of its address).
/// The client is Henge Remote (remote/ in this repository), which reaches a phone on a cable through usbmuxd.
/// Only USB clients by default. With Wi-Fi allowed, a computer on the network gets in once it has been paired with
/// (both show a number, the user agrees on the phone), and everything to and from it is encrypted: RemoteSeal.swift.
///
/// Protocol 1 is the above with the picture as a raw H.264 Annex-B stream (`ffplay tcp://…` shows it). A client that sends type 16 (a = 2) within 0.3 s of connecting gets protocol 2 instead: everything from the
/// phone comes as [type][flags][0][0][length: UInt32 LE] and a body:
///   1 a picture: flags bit 0 keyframe, bit 1 HEVC; body = this phone's clock in microseconds (Int64 LE), then Annex-B
///   2 what is being shared, as JSON: name, running, width, height, and what this build can do
///   3 the answer to a ping: the ping's four bytes, then this phone's clock in microseconds
///   4 text the Mac copied
/// and the client may also send:
///   6 send a keyframe          7 a stream setting: a = 1 bitrate (x, tenths of Mbit/s), 2 frames a second (x),
///   8 ping (x, y: any 4 bytes)     3 format (x: 0 H.264, 1 HEVC), 4 longer side in pixels (x, 0 = the Mac's own)
///   9 a controller's state and 10 text copied on the computer, as [type][a][length: UInt32 LE] and a body.
///   11 the pointer by position: a = buttons down (bit 0 left, 1 right, 2 middle), x and y 0...32767 across the Mac's
///      screen. It moves the Mac's pointer to where the computer's is over the picture, with no capturing.
/// Port 5904 answers one line of JSON (name, running) and hangs up, so a computer can list this phone without taking the
/// stream; with Wi-Fi allowed the phone also announces itself on the network as `_henge._tcp`.
final class RemoteDesktop: ObservableObject {
    static let shared = RemoteDesktop()
    static let enabledKey = "henge.remote.enabled"
    static let headlessKey = "henge.remote.headless"
    static let wifiKey = "henge.remote.wifi"
    static let muteHereKey = "henge.remote.muteHere"
    static let port: UInt16 = 5901
    /// The Mac's sound, for the same client: packets of [length: UInt32][rate: UInt32][channels: UInt16][format: UInt16]
    /// (5 s16, 17 s32, 19 float), little-endian, then that many bytes of interleaved PCM.
    static let audioPort: UInt16 = 5903
    static let statusPort: UInt16 = 5904

    @Published private(set) var connected = false
    @Published private(set) var client = ""
    /// A computer on the network that is not known yet asks to pair: its name, and the number it should show too.
    @Published private(set) var pairing: (name: String, number: String)?
    @Published private(set) var pairedCount = RemoteSeal.paired.count
    /// The user has opened pairing: for two minutes a computer that is not known may ask.
    @Published private(set) var pairingOpen = false

    private let queue = DispatchQueue(label: "henge.remote", qos: .userInteractive)
    private let inputQueue = DispatchQueue(label: "henge.remote.input", qos: .userInteractive)
    private let netQueue = DispatchQueue(label: "henge.remote.net", qos: .userInteractive)
    // queue only
    private var listener: NWListener?
    private var connection: NWConnection?
    private var timer: DispatchSourceTimer?
    private var session: VTCompressionSession?
    private var pool: CVPixelBufferPool?
    private var screen = (w: Int32(0), h: Int32(0))
    private var lastSeq: Int32 = -2
    private var lastSent = 0.0
    private var forceKey = true
    private var hadScreen = false
    private var unsent = 0
    private var seal: RemoteSeal?    // what `connection` is encrypted with; nil over USB
    /// A computer waiting for the user's answer. `number` is what the screen shows, once there is one.
    private var asking: (c: NWConnection, seal: RemoteSeal, name: String, number: String?, rest: Data)?
    private var refusedUntil = 0.0
    private var pairingUntil = 0.0
    private var statusListener: NWListener?
    private var macName = ""
    private var sharing = false      // between start() and stop(): a listener that fails is opened again
    private var deviceName = ""
    // queue only: protocol 2
    private var framed = false       // the client said hello: everything goes out as messages
    private var formDecided = false  // the hello came, or the wait for it ran out
    private var formDeadline = 0.0
    private var bitrate = 15_000_000
    private var fps = 60
    private var hevc = false
    private var maxSide = 0          // the encoded picture's longer side; 0 for the Mac's own, up to 2560
    private var encodingHEVC = false // what `session` was made for
    // audioQueue only: sound has a queue of its own, as `queue` waits on the GPU and would make it stutter
    private let audioQueue = DispatchQueue(label: "henge.remote.audio", qos: .userInteractive)
    private var audioListener: NWListener?
    private var audioConn: NWConnection?
    private var audioTimer: DispatchSourceTimer?
    private var audioSeal: RemoteSeal?
    private var audioUnsent = 0
    private var audioBuf = [UInt8](repeating: 0, count: 65536)
    // inputQueue only
    private var inbox: [UInt8] = []
    private var heldKeys = Set<UInt8>()
    private var heldButtons = Set<Int32>()
    private var padUsed = false
    private var tabletUsed = false

    /// The name of the Mac being shown, for a computer's list of what it can connect to.
    func setMac(name: String) {
        queue.async { self.macName = name }
    }

    func start() {
        let name = UIDevice.current.name
        queue.async { [self] in
            guard !sharing else { return }
            (sharing, deviceName) = (true, name)
            open()
            audioQueue.async { [self] in startAudio() }
        }
    }

    /// (queue) Opens whichever of the two listeners is not there. The announcement on the network rides on the
    /// status port's listener and never on the stream's: when Bonjour fails, the listener it is on fails with
    /// it, and that took the stream's port away in the middle of a session over Wi-Fi.
    private func open() {
        guard sharing else { return }
        if listener == nil {
            listener = listen(Self.port, nil) { [weak self] c in self?.accept(c) }
        }
        if statusListener == nil {
            statusListener = listen(Self.statusPort, service()) { [weak self] c in self?.answerStatus(c) }
        }
    }

    /// "Over Wi-Fi" is not to mean the mobile network too, where the carrier lets connections in.
    private static var offCellular: NWParameters {
        let p = NWParameters.tcp
        p.prohibitedInterfaceTypes = [.cellular]
        return p
    }

    /// (queue) A listener that opens the port again by itself, a second after failing.
    private func listen(_ port: UInt16, _ service: NWListener.Service?, _ handler: @escaping (NWConnection) -> Void) -> NWListener? {
        guard let l = try? NWListener(using: Self.offCellular, on: NWEndpoint.Port(rawValue: port)!) else {
            HengeLog.log("remote", "can't listen on \(port)")
            queue.asyncAfter(deadline: .now() + 1) { [weak self] in self?.open() }
            return nil
        }
        l.newConnectionHandler = handler
        l.service = service
        l.stateUpdateHandler = { [weak self, weak l] state in
            HengeLog.log("remote", "listener \(port): \(state)")
            guard case .failed = state, let self, let l else { return }
            l.cancel()
            self.queue.asyncAfter(deadline: .now() + 1) {
                if self.listener === l { self.listener = nil }
                if self.statusListener === l { self.statusListener = nil }
                self.open()
            }
        }
        l.start(queue: queue)
        return l
    }

    /// (queue) What computers on the network find this phone by, while Wi-Fi is allowed: its name, and the
    /// stream's port (the announcement itself carries the status port's).
    private func service() -> NWListener.Service? {
        guard UserDefaults.standard.bool(forKey: Self.wifiKey) else { return nil }
        return NWListener.Service(name: deviceName, type: "_henge._tcp", txtRecord: NWTXTRecord(["name": deviceName, "port": "\(Self.port)"]))
    }

    /// Wi-Fi was allowed or shut out: start or stop announcing. The listeners stay as they are; one made anew
    /// in the same moment found the port still taken by the one before it and failed.
    func announce() {
        queue.async { [self] in statusListener?.service = service() }
    }

    func stop() {
        queue.async { [self] in
            sharing = false
            listener?.cancel()
            listener = nil
            statusListener?.cancel()
            statusListener = nil
            drop()
        }
        audioQueue.async { [self] in
            audioListener?.cancel()
            audioListener = nil
            dropAudio()
        }
    }

    // MARK: - Sound (audioQueue)

    private func startAudio() {
        guard audioListener == nil else { return }
        do {
            let l = try NWListener(using: Self.offCellular, on: NWEndpoint.Port(rawValue: Self.audioPort)!)
            l.newConnectionHandler = { [weak self] c in self?.acceptAudio(c) }
            l.start(queue: audioQueue)
            audioListener = l
        } catch {
            HengeLog.log("remote", "can't listen on \(Self.audioPort): \(error)")
        }
    }

    /// The same rule as the picture's: USB, or Wi-Fi when allowed and for a computer already paired with.
    private func acceptAudio(_ c: NWConnection) {
        let usb = Self.isLoopback(c.endpoint)
        if !usb && !UserDefaults.standard.bool(forKey: Self.wifiKey) {
            c.cancel()
            return
        }
        c.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .failed, .cancelled: self.audioQueue.async { if c === self.audioConn { self.dropAudio() } }
            default: break
            }
        }
        c.start(queue: audioQueue)
        if usb { promoteAudio(c, nil); return }
        RemoteSeal.accept(c) { [weak self] seal in
            guard let self, let seal else { c.cancel(); return }
            seal.greeting(c) { hello in
                // Pairing happens on the picture's connection; the sound waits for it and comes again.
                guard let hello, hello.knows, RemoteSeal.knows(seal.computer) else { c.cancel(); return }
                seal.send(c, Data([RemoteSeal.go]))
                self.promoteAudio(c, seal)
            }
        }
        audioQueue.asyncAfter(deadline: .now() + 10) { [weak self] in
            if let self, c !== self.audioConn { c.cancel() }
        }
    }

    private func promoteAudio(_ c: NWConnection, _ seal: RemoteSeal?) {
        dropAudio()
        (audioConn, audioSeal) = (c, seal)
        audioUnsent = 0
        let muteHere = UserDefaults.standard.object(forKey: Self.muteHereKey) as? Bool ?? true
        henge_audio_tap(1, muteHere ? 1 : 0)
        HengeLog.log("remote", "sound to \(c.endpoint)\(muteHere ? ", silent here" : "")")
        let t = DispatchSource.makeTimerSource(queue: audioQueue)
        t.schedule(deadline: .now(), repeating: .milliseconds(10), leeway: .milliseconds(1))
        t.setEventHandler { [weak self] in self?.sendAudio() }
        t.resume()
        audioTimer = t
        watchAudio(c)
    }

    /// Nothing comes back on this connection; reading only notices it closing.
    private func watchAudio(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 64) { [weak self] _, _, done, error in
            guard let self else { return }
            if done || error != nil { if c === self.audioConn { self.dropAudio() }; return }
            self.watchAudio(c)
        }
    }

    private func sendAudio() {
        guard let c = audioConn else { return }
        var channels: UInt32 = 0, format: UInt32 = 0, rate: UInt32 = 0
        let n = Int(henge_audio_tap_read(&audioBuf, Int32(audioBuf.count), &channels, &format, &rate))
        guard n > 0, channels > 0, rate > 0 else { return }
        if audioUnsent > 256 * 1024 { return } // the computer is behind: drop this bit rather than fall behind further
        var packet = Data(capacity: 12 + n)
        withUnsafeBytes(of: UInt32(n).littleEndian) { packet.append(contentsOf: $0) }
        withUnsafeBytes(of: rate.littleEndian) { packet.append(contentsOf: $0) }
        withUnsafeBytes(of: UInt16(channels).littleEndian) { packet.append(contentsOf: $0) }
        withUnsafeBytes(of: UInt16(format).littleEndian) { packet.append(contentsOf: $0) }
        packet.append(contentsOf: audioBuf[0..<n])
        audioUnsent += packet.count
        let size = packet.count
        let sent = NWConnection.SendCompletion.contentProcessed { [weak self] _ in
            self?.audioQueue.async { if c === self?.audioConn { self?.audioUnsent = max(0, (self?.audioUnsent ?? 0) - size) } }
        }
        if let audioSeal { audioSeal.send(c, packet, completion: sent) } else { c.send(content: packet, completion: sent) }
    }

    private func dropAudio() {
        audioTimer?.cancel()
        audioTimer = nil
        guard let c = audioConn else { return }
        (audioConn, audioSeal) = (nil, nil)
        c.cancel()
        henge_audio_tap(0, 0)
    }

    /// The "hide on phone" setting changed while a client may be watching.
    func applyHeadless() {
        queue.async { [self] in
            if connection != nil { henge_vm_set_headless(UserDefaults.standard.bool(forKey: Self.headlessKey) ? 1 : 0) }
        }
    }

    /// The phone's Wi-Fi address, to connect to without USB.
    static func wifiAddress() -> String? {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return nil }
        defer { freeifaddrs(list) }
        for p in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let a = p.pointee
            guard String(cString: a.ifa_name) == "en0", let sa = a.ifa_addr, sa.pointee.sa_family == UInt8(AF_INET) else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            if getnameinfo(sa, socklen_t(sa.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                return String(cString: host)
            }
        }
        return nil
    }

    // MARK: - Connection (queue)

    /// USB (iproxy) connections come from usbmux on the phone itself, so from loopback; anything else came over a network.
    static func isLoopback(_ e: NWEndpoint) -> Bool {
        guard case .hostPort(let host, _) = e else { return false }
        switch host {
        case .ipv4(let a): return a.isLoopback
        case .ipv6(let a): return a.isLoopback || a.asIPv4?.isLoopback == true
        default: return false
        }
    }

    private func accept(_ c: NWConnection) {
        let usb = Self.isLoopback(c.endpoint)
        if !usb && !UserDefaults.standard.bool(forKey: Self.wifiKey) {
            HengeLog.log("remote", "refused \(c.endpoint): not over USB, and Wi-Fi is not allowed")
            c.cancel()
            return
        }
        c.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .failed(let error):
                HengeLog.log("remote", "connection to \(c.endpoint) failed: \(error)")
                self.queue.async { if c === self.connection { self.drop() }; self.gone(c) }
            case .cancelled: self.queue.async { if c === self.connection { self.drop() }; self.gone(c) }
            default: break
            }
        }
        // Its own queue: `queue` waits on the GPU for each frame, which under a game would hold up input behind it.
        c.start(queue: netQueue)
        if usb { promote(c, nil, Data()); return }
        // Over a network: the keys first, then who it is. No picture and no input before the phone knows it.
        RemoteSeal.accept(c) { [weak self] seal in
            guard let self, let seal else {
                HengeLog.log("remote", "refused \(c.endpoint): not a Veyron Remote that this build can talk to")
                c.cancel()
                return
            }
            seal.greeting(c) { hello in
                guard let hello else { c.cancel(); return }
                self.queue.async { self.admit(c, seal, knows: hello.knows, name: hello.name, pledge: hello.pledge, rest: hello.rest) }
            }
        }
        queue.asyncAfter(deadline: .now() + 10) { [weak self] in
            if let self, c !== self.connection, c !== self.asking?.c { c.cancel() }
        }
    }

    /// (queue) A computer on the network has shown its key. One the phone has paired with, and that knows the
    /// phone, goes straight in. Any other may only ask while the user has pairing open, one at a time, and
    /// after any attempt that did not end in a pairing none may for ten seconds: each attempt has a one in a
    /// million chance of a number that fools the user, so there must not be many.
    private func admit(_ c: NWConnection, _ seal: RemoteSeal, knows: Bool, name: String, pledge: Data, rest: Data) {
        if knows && RemoteSeal.knows(seal.computer) {
            seal.send(c, Data([RemoteSeal.go]))
            promote(c, seal, rest)
            return
        }
        if let a = asking, a.seal.computer == seal.computer {
            // The same computer again (its key is proven by the handshake): it gave up on its first try, which
            // the phone need not have noticed. The new try takes the old one's place.
            asking = nil
            a.c.cancel()
            DispatchQueue.main.async { self.pairing = nil }
        }
        guard CACurrentMediaTime() < pairingUntil, asking == nil, CACurrentMediaTime() >= refusedUntil else {
            HengeLog.log("remote", "refused \(name) at \(c.endpoint): not paired, and pairing is not open or was just turned down")
            c.cancel()
            return
        }
        // The phone's random bytes go out with the request; the computer's come back and must be the ones it
        // promised in its greeting. Only then is there a number to show.
        let mine = RemoteSeal.random()
        asking = (c, seal, name, nil, Data())
        seal.send(c, Data([RemoteSeal.pair]) + mine)
        seal.take(c, 32, have: rest) { [weak self] answer in
            self?.queue.async {
                guard let self, c === self.asking?.c else { return }
                guard let (theirs, rest) = answer, let number = seal.number(mine: mine, theirs: theirs, pledge: pledge) else {
                    HengeLog.log("remote", "\(name) at \(c.endpoint) did not keep to its part of the pairing")
                    self.asking = nil
                    self.refusedUntil = CACurrentMediaTime() + 10
                    c.cancel()
                    return
                }
                let shown = (name, String(number.prefix(3)) + " " + String(number.suffix(3)))
                self.asking = (c, seal, name, shown.1, rest)
                HengeLog.log("remote", "\(name) at \(c.endpoint) asks to pair")
                DispatchQueue.main.async { self.pairing = shown }
            }
        }
        queue.asyncAfter(deadline: .now() + 60) { [weak self] in
            if let self, c === self.asking?.c { self.answer(false, to: nil) }
        }
    }

    /// The user's answer to a computer asking to pair, and the number that was on the screen when it was given.
    func answerPairing(_ yes: Bool, number: String) {
        queue.async { self.answer(yes, to: number) }
    }

    /// The user allows computers that are not known yet to ask, for two minutes.
    func openPairing() {
        queue.async { self.pairingUntil = CACurrentMediaTime() + 120 }
        pairingOpen = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 120) { [weak self] in
            self?.queue.async {
                guard let self, CACurrentMediaTime() >= self.pairingUntil else { return } // opened again meanwhile
                DispatchQueue.main.async { self.pairingOpen = false }
            }
        }
    }

    private func answer(_ yes: Bool, to number: String?) {
        guard let a = asking else { return }
        // A yes is for the request whose number the user saw, and for no other that took its place since.
        if yes && (a.number == nil || a.number != number) { return }
        asking = nil
        HengeLog.log("remote", "pairing with \(a.name): \(yes ? "agreed" : "refused")")
        if yes {
            RemoteSeal.remember(a.seal.computer, name: a.name)
            pairingUntil = 0 // one computer for each time pairing is opened
            a.seal.send(a.c, Data([RemoteSeal.go]))
            promote(a.c, a.seal, a.rest)
        } else {
            refusedUntil = CACurrentMediaTime() + 10
            a.c.cancel()
        }
        let count = RemoteSeal.paired.count
        DispatchQueue.main.async {
            (self.pairing, self.pairedCount) = (nil, count)
            if yes { self.pairingOpen = false }
        }
    }

    /// (queue) A connection ended: if it was the one asking to pair, the question goes with it.
    private func gone(_ c: NWConnection) {
        guard c === asking?.c else { return }
        asking = nil
        refusedUntil = CACurrentMediaTime() + 10
        DispatchQueue.main.async { self.pairing = nil }
    }

    /// Every computer has to pair again.
    func forgetComputers() {
        RemoteSeal.forgetAll()
        pairedCount = 0
        queue.async { if self.seal != nil { self.drop() } }
        audioQueue.async { if self.audioSeal != nil { self.dropAudio() } }
    }

    /// One client at a time: the newest that got in wins. `rest`: what it sent behind its greeting.
    private func promote(_ c: NWConnection, _ seal: RemoteSeal?, _ rest: Data) {
        drop()
        (connection, self.seal) = (c, seal)
        begin(c)
        if !rest.isEmpty { inputQueue.async { self.handle(rest) } }
        receive(c, seal)
    }

    private func begin(_ c: NWConnection) {
        HengeLog.log("remote", "client \(c.endpoint)")
        unsent = 0
        forceKey = true
        lastSeq = -2
        if UserDefaults.standard.bool(forKey: Self.headlessKey) { henge_vm_set_headless(1) }
        let name = "\(c.endpoint)"
        DispatchQueue.main.async { self.connected = true; self.client = name }
        // Each client starts from the same settings and says what it wants changed.
        (framed, formDecided, formDeadline) = (false, false, CACurrentMediaTime() + 0.3)
        (bitrate, fps, hevc, maxSide) = (15_000_000, 60, false, 0)
        startTimer()
    }

    private func startTimer() {
        timer?.cancel()
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now(), repeating: .nanoseconds(1_000_000_000 / fps), leeway: .milliseconds(1))
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume()
        timer = t
    }

    // MARK: - Protocol 2 (queue)

    private func hello() {
        guard connection != nil, !formDecided else { return } // too late: the raw stream has begun
        (framed, formDecided) = (true, true)
        var w: Int32 = 0, h: Int32 = 0
        let running = henge_vm_stream_frame(nil, &w, &h) >= 0
        let info: [String: Any] = ["name": macName, "running": running, "width": Int(w), "height": Int(h),
                                   "hevc": true, "controller": true, "clipboard": true, "pointer": true]
        if let body = try? JSONSerialization.data(withJSONObject: info) { message(2, body) }
    }

    private func message(_ kind: UInt8, flags: UInt8 = 0, _ body: Data) {
        guard framed, let c = connection else { return }
        var out = Data(capacity: 8 + body.count)
        out.append(contentsOf: [kind, flags, 0, 0])
        withUnsafeBytes(of: UInt32(body.count).littleEndian) { out.append(contentsOf: $0) }
        out.append(body)
        wire(c, out)
    }

    /// (queue) Bytes for the client, encrypted when it is on the network, and counted until they have left.
    private func wire(_ c: NWConnection, _ out: Data) {
        let size = out.count
        unsent += size
        let sent = NWConnection.SendCompletion.contentProcessed { [weak self] _ in
            guard let self else { return }
            self.queue.async { if c === self.connection { self.unsent = max(0, self.unsent - size) } }
        }
        if let seal { seal.send(c, out, completion: sent) } else { c.send(content: out, completion: sent) }
    }

    private func pong(_ echo: [UInt8]) {
        var body = Data(echo)
        withUnsafeBytes(of: Int64(CACurrentMediaTime() * 1_000_000).littleEndian) { body.append(contentsOf: $0) }
        message(3, body)
    }

    private func tune(_ which: UInt8, _ value: Int) {
        switch which {
        case 1: // tenths of a megabit a second
            bitrate = min(max(value, 10), 1000) * 100_000
            if let s = session { VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFNumber) }
        case 2:
            let wanted = min(max(value, 15), 120)
            guard wanted != fps else { return }
            fps = wanted
            if let s = session { VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: fps as CFNumber) }
            if timer != nil { startTimer() }
        case 3:
            guard (value != 0) != hevc else { return }
            hevc = value != 0
            screen = (0, 0) // a new encoder with the next picture
        case 4:
            let side = value == 0 ? 0 : min(max(value, 640), 3840)
            guard side != maxSide else { return }
            maxSide = side
            screen = (0, 0)
        default:
            break
        }
        HengeLog.log("remote", "stream: \(bitrate / 1000) kbit/s, \(fps) fps, \(hevc ? "HEVC" : "H.264"), longer side \(maxSide == 0 ? "as the Mac's" : "\(maxSide)")")
    }

    /// The Mac's clipboard changed: a protocol 2 client keeps its own in step.
    func macCopied(_ text: String) {
        queue.async { self.message(4, Data(text.utf8)) }
    }

    /// Port 5904: whether a Mac is running here and what it is called, then goodbye.
    private func answerStatus(_ c: NWConnection) {
        guard Self.isLoopback(c.endpoint) || UserDefaults.standard.bool(forKey: Self.wifiKey) else {
            c.cancel()
            return
        }
        var w: Int32 = 0, h: Int32 = 0
        let info: [String: Any] = ["name": macName, "running": henge_vm_stream_frame(nil, &w, &h) >= 0]
        c.start(queue: queue)
        c.send(content: try? JSONSerialization.data(withJSONObject: info), contentContext: .finalMessage, isComplete: true,
               completion: .contentProcessed { _ in c.cancel() })
    }

    private func drop() {
        timer?.cancel()
        timer = nil
        let had = connection != nil
        connection?.cancel()
        (connection, seal) = (nil, nil)
        if let s = session { VTCompressionSessionInvalidate(s) }
        session = nil
        pool = nil
        screen = (0, 0)
        guard had else { return }
        henge_vm_set_headless(0)
        inputQueue.async { [self] in
            if padUsed { henge_vm_gamepad(0, nil) } // the controller goes with the computer it was on
            if tabletUsed { henge_vm_tablet(0, -1, 0) } // and so does its pointer, letting go of any button
            (padUsed, tabletUsed) = (false, false)
            for k in heldKeys { henge_vm_key_hid(k, 0) }
            for b in heldButtons { henge_vm_mouse_button(b, 0) }
            heldKeys.removeAll()
            heldButtons.removeAll()
            inbox.removeAll()
        }
        DispatchQueue.main.async { self.connected = false; self.client = "" }
    }

    // MARK: - Video (queue)

    private func tick() {
        guard connection != nil, unsent < 1_000_000 else { return } // the client is behind: skip, don't queue
        if !formDecided { // a moment for the client's hello, which decides what the first byte sent is
            // A computer on the network always says hello, though after a pairing only once its own user has agreed.
            guard seal == nil, CACurrentMediaTime() >= formDeadline else { return }
            formDecided = true
        }
        var w: Int32 = 0, h: Int32 = 0
        let seq = henge_vm_stream_frame(nil, &w, &h)
        guard seq >= 0, w > 0, h > 0 else { hadScreen = false; return }
        if !hadScreen { // a Mac started (again): its display is a new one, which has not been told
            hadScreen = true
            forceKey = true
            if UserDefaults.standard.bool(forKey: Self.headlessKey) { henge_vm_set_headless(1) }
        }
        let now = CACurrentMediaTime()
        if seq == lastSeq && !forceKey && now - lastSent < 1 { return } // nothing new; still a frame a second
        if w != screen.w || h != screen.h { makeEncoder(w, h) }
        guard let session, let pool else { return }
        var made: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &made) == kCVReturnSuccess, let pb = made else { return }
        let drawn = henge_vm_stream_frame(Unmanaged.passUnretained(pb).toOpaque(), &w, &h)
        guard drawn >= 0 else { return }
        lastSeq = drawn
        lastSent = now
        let props: CFDictionary? = forceKey ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        forceKey = false
        VTCompressionSessionEncodeFrame(session, imageBuffer: pb, presentationTimeStamp: CMTime(seconds: now, preferredTimescale: 1_000_000),
                                        duration: .invalid, frameProperties: props, infoFlagsOut: nil) { [weak self] status, _, sample in
            guard status == noErr, let sample, let self else { return }
            self.queue.async { self.send(sample) }
        }
    }

    private func makeEncoder(_ w: Int32, _ h: Int32) {
        if let s = session { VTCompressionSessionInvalidate(s) }
        session = nil
        pool = nil
        screen = (w, h)
        // The hardware encoder tops out around 4K; past 2560 on the long side the stream only gets slower.
        let scale = min(1, Double(maxSide == 0 ? 2560 : min(maxSide, 2560)) / Double(max(w, h)))
        let codec = hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264
        let ew = Int32(Double(w) * scale) & ~1, eh = Int32(Double(h) * scale) & ~1
        let attrs = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                     kCVPixelBufferMetalCompatibilityKey: true,
                     kCVPixelBufferIOSurfacePropertiesKey: [String: Any]()] as CFDictionary
        var s: VTCompressionSession?
        let lowLatency = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true] as CFDictionary
        var err = VTCompressionSessionCreate(allocator: nil, width: ew, height: eh, codecType: codec, encoderSpecification: lowLatency,
                                             imageBufferAttributes: attrs, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        if err != noErr {
            err = VTCompressionSessionCreate(allocator: nil, width: ew, height: eh, codecType: codec, encoderSpecification: nil,
                                             imageBufferAttributes: attrs, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        }
        guard err == noErr, let s else {
            HengeLog.log("remote", "no encoder for \(ew)x\(eh): \(err)")
            return
        }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: hevc ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_High_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: 240 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: fps as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
        session = s
        pool = VTCompressionSessionGetPixelBufferPool(s)
        encodingHEVC = hevc
        forceKey = true
        HengeLog.log("remote", "encoding \(w)x\(h) as \(ew)x\(eh) \(hevc ? "HEVC" : "H.264")")
    }

    /// One encoded frame as Annex-B: parameter sets before each keyframe, start codes for the length prefixes.
    private func send(_ sample: CMSampleBuffer) {
        guard let c = connection, let block = CMSampleBufferGetDataBuffer(sample) else { return }
        let startCode: [UInt8] = [0, 0, 0, 1]
        var out = Data()
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        let keyframe = !((attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
        if keyframe, let format = CMSampleBufferGetFormatDescription(sample) {
            // The two formats keep their parameter sets behind differently named calls of the same shape.
            let hevcStream = CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_HEVC
            func parameterSet(_ i: Int, _ p: UnsafeMutablePointer<UnsafePointer<UInt8>?>?, _ n: UnsafeMutablePointer<Int>?, _ count: UnsafeMutablePointer<Int>?) -> OSStatus {
                if hevcStream {
                    return CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format, parameterSetIndex: i, parameterSetPointerOut: p, parameterSetSizeOut: n,
                                                                              parameterSetCountOut: count, nalUnitHeaderLengthOut: nil)
                }
                return CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: i, parameterSetPointerOut: p, parameterSetSizeOut: n,
                                                                          parameterSetCountOut: count, nalUnitHeaderLengthOut: nil)
            }
            var count = 0
            _ = parameterSet(0, nil, nil, &count)
            for i in 0..<count {
                var p: UnsafePointer<UInt8>?
                var n = 0
                if parameterSet(i, &p, &n, nil) == noErr, let p {
                    out.append(contentsOf: startCode)
                    out.append(p, count: n)
                }
            }
        }
        var bytes = [UInt8](repeating: 0, count: CMBlockBufferGetDataLength(block))
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: bytes.count, destination: &bytes) == kCMBlockBufferNoErr else { return }
        var i = 0
        while i + 4 <= bytes.count {
            let n = Int(bytes[i]) << 24 | Int(bytes[i + 1]) << 16 | Int(bytes[i + 2]) << 8 | Int(bytes[i + 3])
            i += 4
            guard n > 0, i + n <= bytes.count else { break }
            out.append(contentsOf: startCode)
            out.append(contentsOf: bytes[i..<(i + n)])
            i += n
        }
        if framed {
            var body = Data(capacity: 8 + out.count)
            let taken = Int64(CMSampleBufferGetPresentationTimeStamp(sample).seconds * 1_000_000)
            withUnsafeBytes(of: taken.littleEndian) { body.append(contentsOf: $0) }
            body.append(out)
            message(1, flags: (keyframe ? 1 : 0) | (encodingHEVC ? 2 : 0), body)
            return
        }
        wire(c, out)
    }

    // MARK: - Input

    private func receive(_ c: NWConnection, _ seal: RemoteSeal?) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] data, _, done, error in
            guard let self else { return }
            var ended = done || error != nil
            if let data, !data.isEmpty {
                if let seal {
                    if let plain = seal.open(data) { if !plain.isEmpty { self.inputQueue.async { self.handle(plain) } } }
                    else { HengeLog.log("remote", "what \(c.endpoint) sent was not sealed by it"); ended = true }
                } else {
                    self.inputQueue.async { self.handle(data) }
                }
            }
            if ended {
                self.queue.async { if c === self.connection { self.drop() } }
                return
            }
            self.receive(c, seal)
        }
    }

    private func handle(_ data: Data) {
        inbox.append(contentsOf: data)
        var at = 0
        while at + 6 <= inbox.count {
            let type = inbox[at], a = inbox[at + 1]
            if type == 5 { // the computer's name: a bytes after the message
                guard at + 6 + Int(a) <= inbox.count else { break }
                let raw = String(decoding: inbox[(at + 6)..<(at + 6 + Int(a))], as: UTF8.self)
                let name = String(raw.filter { !$0.isNewline && ($0.asciiValue ?? 32) >= 32 }.prefix(63)) // no control characters
                at += 6 + Int(a)
                if !name.isEmpty { DispatchQueue.main.async { self.client = name } }
                continue
            }
            if type == 9 || type == 10 { // protocol 2, with a body: [type][a][length: UInt32 LE]
                let n = Int(inbox[at + 2]) | Int(inbox[at + 3]) << 8 | Int(inbox[at + 4]) << 16 | Int(inbox[at + 5]) << 24
                guard n <= 1 << 20 else { // nothing real is that long: the stream is out of step, so end it
                    inbox.removeAll()
                    queue.async { self.drop() }
                    return
                }
                guard at + 6 + n <= inbox.count else { break }
                let body = Array(inbox[(at + 6)..<(at + 6 + n)])
                at += 6 + n
                if type == 10 {
                    henge_share_set_clipboard(String(decoding: body, as: UTF8.self))
                } else if n >= 16 { // buttons as bits (UInt32), then six axes (Int16), little-endian
                    let buttons = UInt32(body[0]) | UInt32(body[1]) << 8 | UInt32(body[2]) << 16 | UInt32(body[3]) << 24
                    let axes = (0..<6).map { Int16(bitPattern: UInt16(body[4 + $0 * 2]) | UInt16(body[5 + $0 * 2]) << 8) }
                    henge_vm_gamepad(buttons, axes)
                    padUsed = true
                }
                continue
            }
            let x = Int32(Int16(bitPattern: UInt16(inbox[at + 2]) | UInt16(inbox[at + 3]) << 8))
            let y = Int32(Int16(bitPattern: UInt16(inbox[at + 4]) | UInt16(inbox[at + 5]) << 8))
            at += 6
            switch type {
            case 1:
                henge_vm_mouse_move(x, y)
            case 2:
                let b = Int32(a)
                henge_vm_mouse_button(b, x != 0 ? 1 : 0)
                if x != 0 { heldButtons.insert(b) } else { heldButtons.remove(b) }
            case 3:
                henge_vm_mouse_scroll(x, y)
            case 4:
                henge_vm_key_hid(a, x != 0 ? 1 : 0)
                if x != 0 { heldKeys.insert(a) } else { heldKeys.remove(a) }
            case 6:
                queue.async { self.forceKey = true }
            case 7:
                queue.async { self.tune(a, Int(x)) }
            case 8:
                let echo = Array(inbox[(at - 4)..<at])
                queue.async { self.pong(echo) }
            case 11:
                henge_vm_tablet(Int32(a), x, y)
                tabletUsed = true
            case 16:
                queue.async { self.hello() }
            default:
                break
            }
        }
        inbox.removeFirst(at)
    }
}

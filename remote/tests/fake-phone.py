#!/usr/bin/env python3
"""Stands in for Veyron on a phone, so the client can be worked on with no phone at hand.

    python3 tests/fake-phone.py            # the first protocol: raw H.264, as every Veyron build sends
    python3 tests/fake-phone.py --v2       # protocol 2: framed messages, settings, ping, clipboard, status port
    veyron-remote 127.0.0.1:15901

Needs ffmpeg on the PATH. The picture is ffmpeg's moving test card with its frame counter; the sound is a quiet tone.
Everything the client sends is printed.

The client reaches it by address, which to the client is a phone on the network: so the connection is sealed
as ios/Henge/RemoteSeal.swift has it (needs Python's `cryptography`). A client that says it knows this phone is
let in at once; any other is asked to pair, the number is printed, and two seconds later the "user" agrees.
"""
import hashlib, hmac, json, math, os, socket, struct, subprocess, sys, threading, time

from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

V2 = "--v2" in sys.argv
PORT = 15901
SIZE, FPS = "1920x1200", 60


IDENTITY = X25519PrivateKey.from_private_bytes(hashlib.sha256(b"fake phone").digest())  # the same phone every run


def raw(key):
    return key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)


class Noise:
    """Noise_XX_25519_ChaChaPoly_SHA256 as the one who answers, step by step as the Noise specification has
    it (sections 5.1 to 5.3), which is how the phone's Swift does it: Apple ships no Noise to call."""

    def __init__(self, prologue):
        self.h = self.ck = b"Noise_XX_25519_ChaChaPoly_SHA256"  # 32 bytes, so it is the first hash as it stands
        self.k, self.n = None, 0
        self.mix_hash(prologue)

    def mix_hash(self, data):
        self.h = hashlib.sha256(self.h + data).digest()

    @staticmethod
    def hkdf(ck, material):
        temp = hmac.new(ck, material, hashlib.sha256).digest()
        one = hmac.new(temp, b"\x01", hashlib.sha256).digest()
        return one, hmac.new(temp, one + b"\x02", hashlib.sha256).digest()

    def mix_key(self, material):
        self.ck, key = self.hkdf(self.ck, material)
        self.k, self.n = ChaCha20Poly1305(key), 0

    def seal(self, plain):  # EncryptAndHash
        sealed = plain if self.k is None else self.k.encrypt(bytes(4) + self.n.to_bytes(8, "little"), plain, self.h)
        self.n += self.k is not None
        self.mix_hash(sealed)
        return sealed

    def open(self, sealed):  # DecryptAndHash
        plain = sealed if self.k is None else self.k.decrypt(bytes(4) + self.n.to_bytes(8, "little"), sealed, self.h)
        self.n += self.k is not None
        self.mix_hash(sealed)
        return plain


class Sealed:
    """A socket after the handshake: sendall and read as before, in Noise's transport messages."""

    def __init__(self, c):
        f = c.makefile("rb")
        if f.read(4) != b"VYN2":
            raise OSError("not a Veyron Remote")
        noise = Noise(b"VYN2")
        theirs = f.read(32)  # -> e
        noise.mix_hash(theirs)
        noise.open(b"")
        their_fresh, fresh = X25519PublicKey.from_public_bytes(theirs), X25519PrivateKey.generate()
        noise.mix_hash(raw(fresh))  # <- e, ee, s, es
        noise.mix_key(fresh.exchange(their_fresh))
        mine = noise.seal(raw(IDENTITY))
        noise.mix_key(IDENTITY.exchange(their_fresh))
        c.sendall(raw(fresh) + mine + noise.seal(b""))
        third = f.read(64)  # -> s, se
        self.computer = noise.open(third[:48])
        noise.mix_key(fresh.exchange(X25519PublicKey.from_public_bytes(self.computer)))
        noise.open(third[48:])
        to_phone, to_computer = Noise.hkdf(noise.ck, b"")
        self.c, self.f, self.lock, self.binding = c, f, threading.Lock(), noise.h
        self.opening, self.sealing, self.opened, self.sent, self.plain = ChaCha20Poly1305(to_phone), ChaCha20Poly1305(to_computer), 0, 0, b""
        flags, n = self.read(2)
        self.knows, self.name, self.pledge = bool(flags & 1), self.read(n).decode(errors="replace"), self.read(32)

    def pair(self):
        """Asks to pair: this side's random bytes go with the request, the computer's come back and must be
        the ones it promised. Returns the six digits both screens show."""
        mine = os.urandom(32)
        self.sendall(b"\xa2" + mine)
        theirs = self.read(32)
        if hashlib.sha256(theirs).digest() != self.pledge:
            raise OSError("the computer did not keep its promise")
        return "%06d" % (int.from_bytes(hashlib.sha256(b"veyron pairing" + self.binding + theirs + mine).digest()[:4], "big") % 1_000_000)

    def sendall(self, data):
        with self.lock:
            out = b""
            for i in range(0, len(data), 60000):
                sealed = self.sealing.encrypt(bytes(4) + self.sent.to_bytes(8, "little"), data[i:i + 60000], None)
                self.sent += 1
                out += struct.pack(">H", len(sealed)) + sealed
            self.c.sendall(out)

    def read(self, n):
        while len(self.plain) < n:
            head = self.f.read(2)
            if len(head) < 2:
                break
            sealed = self.f.read(struct.unpack(">H", head)[0])
            self.plain += self.opening.decrypt(bytes(4) + self.opened.to_bytes(8, "little"), sealed, None)
            self.opened += 1
        out, self.plain = self.plain[:n], self.plain[n:]
        return out

    def close(self):
        self.c.close()


def serve(port, handler, sealed=True):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", port))
    s.listen(1)
    while True:
        c, _ = s.accept()
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

        def run(c=c):
            try:
                handler(Sealed(c) if sealed else c)
            except Exception as e:
                print(f"port {port}: {e}", flush=True)
                c.close()

        threading.Thread(target=run, daemon=True).start()


def frames():
    """ffmpeg's H.264, one picture at a time: cut where an access unit delimiter starts."""
    cmd = ["ffmpeg", "-loglevel", "error", "-re", "-f", "lavfi", "-i", f"testsrc2=size={SIZE}:rate={FPS}",
           "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-bf", "0", "-g", "120",
           "-x264-params", "aud=1", "-pix_fmt", "yuv420p", "-f", "h264", "-"]
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE)
    buf, aud = b"", b"\x00\x00\x00\x01\x09"
    try:
        while True:
            chunk = p.stdout.read1(1 << 16)
            if not chunk:
                return
            buf += chunk
            while True:
                i = buf.find(aud, 5)
                if i < 0:
                    break
                yield buf[:i]
                buf = buf[i:]
    finally:
        p.kill()


def video(c):
    known = c.knows and c.computer in PAIRED
    print(f"video: {c.name} connected, {'known' if known else 'not known: pairing'}", flush=True)
    if not known:
        number = c.pair()
        print(f"  pairing number {number[:3]} {number[3:]}; agreeing in two seconds", flush=True)
        time.sleep(2)
        PAIRED.add(c.computer)
    c.sendall(b"\xa1")
    framed = [False]
    lock = threading.Lock()

    def message(kind, flags, body):
        with lock:
            c.sendall(struct.pack("<BBHI", kind, flags, 0, len(body)) + body)

    def reader():
        f = c
        while True:
            m = f.read(6)
            if len(m) < 6:
                return
            kind, a, x, y = struct.unpack("<BBhh", m)
            if kind == 5:
                print("  name:", f.read(a).decode(errors="replace"), flush=True)
            elif kind == 16:
                framed[0] = V2
                print(f"  hello, protocol {a}" + ("" if V2 else " (ignored: this is the first protocol)"), flush=True)
            elif kind == 8:
                message(3, 0, m[2:6] + struct.pack("<q", time.monotonic_ns() // 1000))
            elif kind in (9, 10):
                n = struct.unpack("<I", m[2:6])[0]
                body = f.read(n)
                print("  controller" if kind == 9 else "  clipboard", body.hex() if kind == 9 else body.decode(errors="replace"), flush=True)
                if kind == 10:
                    message(4, 0, ("Mac says: " + body.decode(errors="replace")).encode())
            elif kind != 1:
                print(f"  message {kind}: a={a} x={x} y={y}", flush=True)

    threading.Thread(target=reader, daemon=True).start()
    # As the phone does for a computer on the network: wait for its hello, which after a pairing comes only
    # once the computer's own user has agreed. (Without --v2 the hello is not heeded, and a moment is enough.)
    while V2 and not framed[0]:
        time.sleep(0.02)
    time.sleep(0.0 if V2 else 0.3)
    try:
        if framed[0]:
            message(2, 0, json.dumps({"name": "Test Mac", "running": True, "width": 1920, "height": 1200, "hevc": True, "controller": True, "clipboard": True, "pointer": True}).encode())
        for picture in frames():
            if framed[0]:
                key = b"\x00\x00\x00\x01\x67" in picture[:64] or b"\x00\x00\x00\x01\x27" in picture[:64]
                message(1, 1 if key else 0, struct.pack("<q", time.monotonic_ns() // 1000) + picture)
            else:
                c.sendall(picture)
    except OSError:
        pass
    print("video: client gone", flush=True)


def sound(c):
    if not (c.knows and c.computer in PAIRED):
        c.close()
        return
    c.sendall(b"\xa1")
    rate, t = 48000, 0
    try:
        while True:
            n = rate // 100
            pcm = b"".join(struct.pack("<hh", *(int(1500 * math.sin(2 * math.pi * 330 * (t + i) / rate)),) * 2) for i in range(n))
            c.sendall(struct.pack("<IIHH", len(pcm), rate, 2, 5) + pcm)
            t += n
            time.sleep(0.01)
    except OSError:
        pass


def status(c):
    c.sendall(json.dumps({"name": "Test Mac", "running": True}).encode())
    c.close()


PAIRED = set()
threading.Thread(target=serve, args=(PORT + 2, sound), daemon=True).start()
if V2:
    threading.Thread(target=serve, args=(PORT + 3, status, False), daemon=True).start()
print(f"fake phone on 127.0.0.1:{PORT}, protocol {'2' if V2 else '1'}", flush=True)
serve(PORT, video)

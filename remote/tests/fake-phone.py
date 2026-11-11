#!/usr/bin/env python3
"""Stands in for Henge on a phone, so the client can be worked on with no phone at hand.

    python3 tests/fake-phone.py            # the first protocol: raw H.264, as every Henge build sends
    python3 tests/fake-phone.py --v2       # protocol 2: framed messages, settings, ping, clipboard, status port
    henge-remote 127.0.0.1:15901

Needs ffmpeg on the PATH. The picture is ffmpeg's moving test card with its frame counter; the sound is a quiet tone.
Everything the client sends is printed.
"""
import json, math, socket, struct, subprocess, sys, threading, time

V2 = "--v2" in sys.argv
PORT = 15901
SIZE, FPS = "1920x1200", 60


def serve(port, handler):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", port))
    s.listen(1)
    while True:
        c, _ = s.accept()
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        threading.Thread(target=handler, args=(c,), daemon=True).start()


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
    print("video: client connected", flush=True)
    framed = [False]
    lock = threading.Lock()

    def message(kind, flags, body):
        with lock:
            c.sendall(struct.pack("<BBHI", kind, flags, 0, len(body)) + body)

    def reader():
        f = c.makefile("rb")
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
    time.sleep(0.3)  # as the phone does: wait a moment for the hello before choosing the form
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


threading.Thread(target=serve, args=(PORT + 2, sound), daemon=True).start()
if V2:
    threading.Thread(target=serve, args=(PORT + 3, status), daemon=True).start()
print(f"fake phone on 127.0.0.1:{PORT}, protocol {'2' if V2 else '1'}", flush=True)
serve(PORT, video)

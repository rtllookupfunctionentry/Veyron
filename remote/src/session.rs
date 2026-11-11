//! One connection to a Mac in Henge: the picture and sound in, decoded; mouse, keyboard, controller, clipboard
//! and stream settings out. The protocol is described in ios/Henge/RemoteDesktop.swift.
//!
//! Two forms of it exist. Every Henge build understands the first: raw H.264 one way, 6-byte input messages
//! the other. Builds with protocol 2 answer the hello below by wrapping everything they send in
//! [type][flags][0][0][length u32] messages, which adds timestamps, stream settings, the clipboard and the
//! controller. Which one the phone speaks shows in its first byte: a raw stream starts with a zero.
use crate::usbmux::{self, Conn};
use sdl3_sys::everything::*;
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PORT: u16 = 5901;
/// Sound is a connection of its own, two ports up, so a large picture never holds it back.
const SOUND: u16 = 2;
/// Protocol 2 only: answers "is a Mac running, and what is it called" without taking the stream.
const STATUS: u16 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Target {
    Usb { udid: String },
    Net { host: String, port: u16, code: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    /// Connected, and the phone has sent nothing yet: no Mac is running there, or it is starting.
    Waiting,
    Live,
    Ended,
}

/// What the PC asks of the phone's encoder. Protocol 2 only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub mbps: u32,
    pub fps: u32,
    pub hevc: bool,
    /// The longer side of the encoded picture, in pixels; 0 for the Mac's own size (up to the phone's limit).
    pub max_side: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { mbps: 15, fps: 60, hevc: false, max_side: 0 }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub name: String,
    pub running: bool,
    pub width: u32,
    pub height: u32,
    pub hevc: bool,
    pub controller: bool,
    pub clipboard: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub fps: f32,
    pub mbps: f32,
    pub decode_ms: f32,
    pub rtt_ms: Option<f32>,
    /// From the phone taking the Mac's picture to this PC having decoded it. Protocol 2 only.
    pub latency_ms: Option<f32>,
    pub decoder: String,
    pub width: i32,
    pub height: i32,
}

/// The newest decoded picture, as the planes FFmpeg gave. `serial` goes up with each one.
#[derive(Default)]
pub struct Frame {
    pub planes: [Vec<u8>; 3],
    pub stride: [i32; 3],
    pub nplanes: i32,
    pub width: i32,
    pub height: i32,
    pub bt601: bool,
    pub full_range: bool,
    pub serial: u64,
}

pub struct Options {
    pub hardware: bool,
    pub sound: bool,
    pub tuning: Tuning,
    /// What the phone shows it is streaming to.
    pub name: String,
    /// The SDL event that wakes the window when a picture or a change of state is ready.
    pub wake: u32,
}

pub struct Session {
    pub target: Target,
    pub frame: Mutex<Frame>,
    phase: Mutex<(Phase, String)>,
    stats: Mutex<Stats>,
    info: Mutex<Option<ServerInfo>>,
    clipboard: Mutex<Option<String>>,
    writer: Mutex<Option<Conn>>,
    sound: Mutex<Option<Conn>>,
    framed: AtomicBool,
    closed: AtomicBool,
    gain: AtomicU32,
    frames: AtomicU32,
    wake: u32,
}

// ---- FFmpeg, through src/decode.c ----

#[repr(C)]
struct HdFrame {
    data: [*const u8; 3],
    linesize: [c_int; 3],
    width: c_int,
    height: c_int,
    planes: c_int,
    bt601: c_int,
    full_range: c_int,
}

unsafe extern "C" {
    fn hd_open(hevc: c_int, hardware: c_int, parse: c_int) -> *mut c_void;
    fn hd_name(d: *const c_void) -> *const c_char;
    fn hd_feed(d: *mut c_void, data: *const u8, len: c_int, on_frame: extern "C" fn(*mut c_void, *const HdFrame), user: *mut c_void) -> c_int;
    fn hd_close(d: *mut c_void);
}

struct Decoder(*mut c_void);

impl Decoder {
    fn open(hevc: bool, hardware: bool, parse: bool) -> io::Result<Decoder> {
        let d = unsafe { hd_open(hevc as c_int, hardware as c_int, parse as c_int) };
        if d.is_null() { Err(io::Error::other("this system's FFmpeg has no decoder for the stream")) } else { Ok(Decoder(d)) }
    }

    fn name(&self) -> String {
        unsafe { CStr::from_ptr(hd_name(self.0)) }.to_string_lossy().into_owned()
    }

    /// Returns how many pictures failed to decode.
    fn feed(&mut self, data: &[u8], to: &Session) -> i32 {
        unsafe { hd_feed(self.0, data.as_ptr(), data.len() as c_int, on_frame, to as *const Session as *mut c_void) }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { hd_close(self.0) }
    }
}

extern "C" fn on_frame(user: *mut c_void, f: *const HdFrame) {
    let (s, f) = unsafe { (&*(user as *const Session), &*f) };
    {
        let mut out = s.frame.lock().unwrap();
        for i in 0..f.planes as usize {
            let rows = if i == 0 { f.height } else { (f.height + 1) / 2 };
            let len = (f.linesize[i] * rows) as usize;
            out.planes[i].resize(len, 0);
            unsafe { std::ptr::copy_nonoverlapping(f.data[i], out.planes[i].as_mut_ptr(), len) };
            out.stride[i] = f.linesize[i];
        }
        out.nplanes = f.planes;
        out.width = f.width;
        out.height = f.height;
        out.bt601 = f.bt601 != 0;
        out.full_range = f.full_range != 0;
        out.serial += 1;
    }
    s.frames.fetch_add(1, Ordering::Relaxed);
    s.wake();
}

// ---- the connection ----

fn dial(target: &Target, offset: u16) -> io::Result<Conn> {
    match target {
        Target::Usb { udid } => {
            let dev = usbmux::devices()?.into_iter().find(|d| &d.udid == udid);
            let dev = dev.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the phone is not plugged in"))?;
            usbmux::connect(dev.id, PORT + offset)
        }
        Target::Net { host, port, code } => {
            let addr = (host.as_str(), port + offset).to_socket_addrs()?.next();
            let addr = addr.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such address"))?;
            let s = TcpStream::connect_timeout(&addr, Duration::from_secs(4))?;
            let _ = s.set_nodelay(true);
            let mut c = Conn::Tcp(s);
            if !code.is_empty() {
                // Over Wi-Fi the phone wants the six digits it shows before anything else.
                c.write_all(&format!("{code:0>6}").as_bytes()[..6])?;
            }
            Ok(c)
        }
    }
}

/// Whether a Mac is running on that phone and what it is called, without taking the stream from anyone.
/// None from a Henge that only has the first protocol, or when Henge is not sharing.
pub fn status(target: &Target) -> Option<ServerInfo> {
    let c = dial(target, STATUS).ok()?;
    c.set_read_timeout(Some(Duration::from_secs(2)));
    let mut text = String::new();
    c.take(4096).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

fn explain(target: &Target, e: &io::Error) -> String {
    let usb = matches!(target, Target::Usb { .. });
    match e.kind() {
        io::ErrorKind::ConnectionRefused if usb => "Henge isn't sharing a screen. Open the Mac in Henge and turn on Remote Desktop in its menu.".into(),
        io::ErrorKind::ConnectionRefused => "Nothing answers at that address. Check that Remote Desktop and Wi-Fi are both on in Henge.".into(),
        io::ErrorKind::NotFound if usb => "The iPhone is no longer plugged in.".into(),
        io::ErrorKind::UnexpectedEof if !usb => "The phone closed the connection. Check the code against the one Henge shows.".into(),
        io::ErrorKind::UnexpectedEof => "Henge stopped sharing the screen.".into(),
        io::ErrorKind::TimedOut => "The phone did not answer in time.".into(),
        _ => format!("The connection was lost ({e})."),
    }
}

impl Session {
    pub fn open(target: Target, opts: Options) -> Arc<Session> {
        let s = Arc::new(Session {
            target,
            frame: Mutex::default(),
            phase: Mutex::new((Phase::Connecting, String::new())),
            stats: Mutex::default(),
            info: Mutex::default(),
            clipboard: Mutex::default(),
            writer: Mutex::default(),
            sound: Mutex::default(),
            framed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            gain: AtomicU32::new(1f32.to_bits()),
            frames: AtomicU32::new(0),
            wake: opts.wake,
        });
        if opts.sound {
            let s = s.clone();
            std::thread::spawn(move || s.run_sound());
        }
        let v = s.clone();
        std::thread::spawn(move || {
            let reason = match v.run_video(&opts) {
                Ok(()) => "Henge stopped sharing the screen.".to_string(),
                Err(e) => explain(&v.target, &e),
            };
            *v.phase.lock().unwrap() = (Phase::Ended, reason);
            v.close();
            v.wake();
        });
        s
    }

    pub fn phase(&self) -> (Phase, String) {
        self.phase.lock().unwrap().clone()
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }

    pub fn info(&self) -> Option<ServerInfo> {
        self.info.lock().unwrap().clone()
    }

    /// Whether the phone speaks protocol 2: stream settings, clipboard and controller only exist there.
    pub fn framed(&self) -> bool {
        self.framed.load(Ordering::Relaxed)
    }

    /// What was copied in the Mac since the last call.
    pub fn take_clipboard(&self) -> Option<String> {
        self.clipboard.lock().unwrap().take()
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        for c in [&self.writer, &self.sound] {
            if let Some(c) = c.lock().unwrap().as_ref() {
                c.shutdown();
            }
        }
    }

    fn wake(&self) {
        unsafe {
            let mut e: SDL_Event = std::mem::zeroed();
            e.user.r#type = self.wake;
            SDL_PushEvent(&mut e);
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(c) = self.writer.lock().unwrap().as_mut() {
            if c.write_all(bytes).is_err() {
                c.shutdown(); // the reader notices and ends the session
            }
        }
    }

    /// A 6-byte message: 1 move (x, y), 2 button (a, x = down), 3 scroll (x, y), 4 key (a = HID usage, x = down),
    /// and with protocol 2: 6 send a keyframe, 7 stream setting (a = which, x = value), 8 ping (x, y = a clock).
    pub fn send(&self, kind: u8, a: u8, x: i32, y: i32) {
        let x = (x.clamp(-32768, 32767) as i16).to_le_bytes();
        let y = (y.clamp(-32768, 32767) as i16).to_le_bytes();
        self.write(&[kind, a, x[0], x[1], y[0], y[1]]);
    }

    /// A message with a body: [kind][a][length u32] then the bytes. 9 controller state, 10 clipboard text.
    /// Dropped for a phone without protocol 2, which would read the body as input.
    pub fn send_long(&self, kind: u8, a: u8, body: &[u8]) {
        if !self.framed() {
            return;
        }
        let mut m = Vec::with_capacity(6 + body.len());
        m.extend_from_slice(&[kind, a]);
        m.extend_from_slice(&(body.len() as u32).to_le_bytes());
        m.extend_from_slice(body);
        self.write(&m);
    }

    pub fn tune(&self, t: &Tuning) {
        if !self.framed() {
            return;
        }
        self.send(7, 1, (t.mbps * 10) as i32, 0); // in tenths of a megabit
        self.send(7, 2, t.fps as i32, 0);
        self.send(7, 3, t.hevc as i32, 0);
        self.send(7, 4, t.max_side as i32, 0);
    }

    fn run_video(&self, opts: &Options) -> io::Result<()> {
        let mut conn = dial(&self.target, 0)?;
        *self.writer.lock().unwrap() = Some(conn.try_clone()?);
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        // The hello goes first: a phone that knows it answers in messages from its first byte, and an older
        // one skips a type it has never heard of. Then this computer's name, for the phone to show.
        self.send(16, 2, 0, 0);
        let name = &opts.name.as_bytes()[..opts.name.len().min(63)];
        self.write(&[&[5, name.len() as u8, 0, 0, 0, 0], name].concat());
        self.phase.lock().unwrap().0 = Phase::Waiting;
        self.wake();
        let mut first = [0u8; 1];
        conn.read_exact(&mut first)?;
        self.phase.lock().unwrap().0 = Phase::Live;
        self.wake();
        if first[0] == 0 { self.read_raw(conn, opts) } else { self.read_framed(conn, first[0], opts) }
    }

    /// The first protocol: nothing but H.264, cut into pictures by FFmpeg's parser.
    fn read_raw(&self, mut conn: Conn, opts: &Options) -> io::Result<()> {
        let mut dec = Decoder::open(false, opts.hardware, true)?;
        let mut meter = Meter::new(dec.name());
        eprintln!("henge-remote: decoding with {}", dec.name());
        dec.feed(&[0], self);
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = conn.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            let began = Instant::now();
            dec.feed(&buf[..n], self);
            meter.add(n, began.elapsed());
            meter.publish(self);
        }
    }

    fn read_framed(&self, mut conn: Conn, first: u8, opts: &Options) -> io::Result<()> {
        self.framed.store(true, Ordering::Relaxed);
        self.tune(&opts.tuning);
        let epoch = Instant::now();
        let mut dec: Option<(bool, Decoder)> = None;
        let mut meter = Meter::new(String::new());
        let (mut pinged, mut asked_key) = (Instant::now() - Duration::from_secs(5), Instant::now());
        let mut offset_us: Option<i64> = None; // the phone's clock minus this one's
        let mut head = [first, 0, 0, 0, 0, 0, 0, 0];
        conn.read_exact(&mut head[1..])?;
        let mut body = Vec::new();
        loop {
            let (kind, flags) = (head[0], head[1]);
            let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            if len > 64 << 20 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "the phone sent a message of impossible size"));
            }
            body.resize(len, 0);
            conn.read_exact(&mut body)?;
            match kind {
                // A picture: flags bit 0 keyframe, bit 1 HEVC; the phone's clock in microseconds, then Annex-B.
                1 if len > 8 => {
                    let hevc = flags & 2 != 0;
                    if dec.as_ref().is_none_or(|(h, _)| *h != hevc) {
                        let d = Decoder::open(hevc, opts.hardware, false)?;
                        meter.decoder = d.name();
                        dec = Some((hevc, d));
                    }
                    let d = &mut dec.as_mut().unwrap().1;
                    let began = Instant::now();
                    let errors = d.feed(&body[8..], self);
                    meter.add(len, began.elapsed());
                    meter.decoder = d.name(); // it changes if the GPU turned the stream down
                    if let Some(off) = offset_us {
                        let taken = i64::from_le_bytes(body[..8].try_into().unwrap());
                        meter.latency_us += epoch.elapsed().as_micros() as i64 + off - taken;
                    }
                    if errors > 0 && asked_key.elapsed() > Duration::from_millis(500) {
                        asked_key = Instant::now();
                        self.send(6, 0, 0, 0);
                    }
                }
                2 => {
                    if let Ok(info) = serde_json::from_slice::<ServerInfo>(&body) {
                        *self.info.lock().unwrap() = Some(info);
                        self.wake();
                    }
                }
                // The answer to a ping: the four bytes sent, then the phone's clock in microseconds.
                3 if len >= 12 => {
                    let now = epoch.elapsed();
                    let sent_ms = u32::from_le_bytes(body[..4].try_into().unwrap());
                    let rtt_ms = (now.as_millis() as u32).wrapping_sub(sent_ms);
                    if rtt_ms < 5000 {
                        let phone = i64::from_le_bytes(body[4..12].try_into().unwrap());
                        offset_us = Some(phone - (now.as_micros() as i64 - rtt_ms as i64 * 500));
                        meter.rtt_ms = Some(rtt_ms as f32);
                    }
                }
                4 => {
                    *self.clipboard.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
                    self.wake();
                }
                _ => {}
            }
            if pinged.elapsed() > Duration::from_secs(1) {
                pinged = Instant::now();
                let ms = epoch.elapsed().as_millis() as u32;
                self.write(&[&[8u8, 0][..], &ms.to_le_bytes()].concat());
            }
            meter.publish(self);
            conn.read_exact(&mut head)?;
        }
    }

    /// The Mac's sound: packets of [length u32][rate u32][channels u16][format u16] and PCM, into SDL.
    /// More than a fifth of a second waiting to be played means it fell behind the picture: skip ahead.
    fn run_sound(&self) {
        let Ok(mut conn) = dial(&self.target, SOUND) else { return };
        match conn.try_clone() {
            Ok(c) => *self.sound.lock().unwrap() = Some(c),
            Err(_) => return,
        }
        let mut stream: *mut SDL_AudioStream = std::ptr::null_mut();
        let mut shape = (0u32, 0u16, 0u16);
        let mut head = [0u8; 12];
        let mut data = Vec::new();
        while !self.closed.load(Ordering::Relaxed) && conn.read_exact(&mut head).is_ok() {
            let len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
            let rate = u32::from_le_bytes(head[4..8].try_into().unwrap());
            let channels = u16::from_le_bytes([head[8], head[9]]);
            let format = u16::from_le_bytes([head[10], head[11]]);
            if len > 1 << 20 {
                break;
            }
            data.resize(len, 0);
            if conn.read_exact(&mut data).is_err() {
                break;
            }
            let (sdl_format, sample) = match format {
                5 => (SDL_AUDIO_S16LE, 2),
                17 => (SDL_AUDIO_S32LE, 4),
                19 => (SDL_AUDIO_F32LE, 4),
                _ => continue,
            };
            unsafe {
                if shape != (rate, channels, format) {
                    if !stream.is_null() {
                        SDL_DestroyAudioStream(stream);
                    }
                    let spec = SDL_AudioSpec { format: sdl_format, channels: channels as c_int, freq: rate as c_int };
                    stream = SDL_OpenAudioDeviceStream(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, None, std::ptr::null_mut());
                    if stream.is_null() {
                        return;
                    }
                    SDL_ResumeAudioStreamDevice(stream);
                    shape = (rate, channels, format);
                }
                SDL_SetAudioStreamGain(stream, f32::from_bits(self.gain.load(Ordering::Relaxed)));
                let second = (rate * channels as u32 * sample) as c_int;
                if SDL_GetAudioStreamQueued(stream) > second / 5 {
                    SDL_ClearAudioStream(stream);
                }
                SDL_PutAudioStreamData(stream, data.as_ptr() as *const c_void, len as c_int);
            }
        }
        if !stream.is_null() {
            unsafe { SDL_DestroyAudioStream(stream) };
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// Adds up a second of the stream and turns it into the numbers the overlay shows.
struct Meter {
    since: Instant,
    bytes: usize,
    pictures: u32,
    decode: Duration,
    latency_us: i64,
    rtt_ms: Option<f32>,
    decoder: String,
}

impl Meter {
    fn new(decoder: String) -> Meter {
        Meter { since: Instant::now(), bytes: 0, pictures: 0, decode: Duration::ZERO, latency_us: 0, rtt_ms: None, decoder }
    }

    fn add(&mut self, bytes: usize, decode: Duration) {
        self.bytes += bytes;
        self.pictures += 1;
        self.decode += decode;
    }

    fn publish(&mut self, s: &Session) {
        let secs = self.since.elapsed().as_secs_f32();
        if secs < 1.0 {
            return;
        }
        let shown = s.frames.swap(0, Ordering::Relaxed);
        let (width, height) = {
            let f = s.frame.lock().unwrap();
            (f.width, f.height)
        };
        let per = shown.max(1) as f32; // per picture shown, which in the raw protocol is not per read
        *s.stats.lock().unwrap() = Stats {
            fps: shown as f32 / secs,
            mbps: self.bytes as f32 * 8.0 / 1e6 / secs,
            decode_ms: self.decode.as_secs_f32() * 1000.0 / per,
            rtt_ms: self.rtt_ms,
            latency_ms: (self.latency_us != 0).then(|| self.latency_us as f32 / 1000.0 / per),
            decoder: self.decoder.clone(),
            width,
            height,
        };
        (self.since, self.bytes, self.pictures, self.decode, self.latency_us) = (Instant::now(), 0, 0, Duration::ZERO, 0);
    }
}

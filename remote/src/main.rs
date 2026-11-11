//! Veyron Remote: shows the Mac that Veyron runs on an iPhone and sends it this computer's mouse, keyboard
//! and controller. One window, SDL for everything the system does, egui for what is drawn over the picture.
//!
//!     veyron-remote                       look for phones, show the list
//!     veyron-remote 192.168.1.20:5901 --code 123456     connect straight away
//!     veyron-remote --background          start hidden and wait for a Mac (what "Start at login" runs)
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod discover;
mod session;
mod ui;
mod usbmux;
mod video;

use config::Config;
use discover::Discovery;
use egui_glow::glow::{self, HasContext};
use sdl3_sys::everything::*;
use session::{Options, Phase, Session, Target};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let background = args.iter().any(|a| a == "--background");
    let code = args.iter().position(|a| a == "--code").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let address = args.iter().find(|a| !a.starts_with("--") && **a != code).cloned();
    unsafe { run(background, address.map(|a| (a, code))) }
}

fn this_computer() -> String {
    let name = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok();
    let name = name.or_else(|| std::fs::read_to_string("/etc/hostname").ok()).unwrap_or_else(|| "Computer".into());
    name.trim().split('.').next().unwrap_or("Computer").to_string()
}

/// The window's size in pixels, and how many of them make one of egui's points.
unsafe fn metrics(window: *mut SDL_Window) -> ((i32, i32), f32) {
    unsafe {
        let (mut w, mut h) = (0, 0);
        SDL_GetWindowSizeInPixels(window, &mut w, &mut h);
        let scale = SDL_GetWindowDisplayScale(window);
        ((w.max(1), h.max(1)), if scale > 0.0 { scale } else { 1.0 })
    }
}

/// SDL gives the mouse in window units, which are pixels on some systems and points on others.
unsafe fn to_points(window: *mut SDL_Window, x: f32, y: f32) -> egui::Pos2 {
    unsafe {
        let ((pw, ph), ppp) = metrics(window);
        let (mut w, mut h) = (0, 0);
        SDL_GetWindowSize(window, &mut w, &mut h);
        egui::pos2(x * pw as f32 / w.max(1) as f32 / ppp, y * ph as f32 / h.max(1) as f32 / ppp)
    }
}

fn modifiers(m: SDL_Keymod) -> egui::Modifiers {
    let has = |bits: SDL_Keymod| m.0 & bits.0 != 0;
    let (ctrl, gui) = (has(SDL_KMOD_CTRL), has(SDL_KMOD_GUI));
    egui::Modifiers { alt: has(SDL_KMOD_ALT), ctrl, shift: has(SDL_KMOD_SHIFT), mac_cmd: cfg!(target_os = "macos") && gui, command: if cfg!(target_os = "macos") { gui } else { ctrl } }
}

/// The keys egui's text fields and dialogs act on; the rest reach it as text.
fn egui_key(key: SDL_Keycode) -> Option<egui::Key> {
    use egui::Key;
    Some(match key {
        SDLK_BACKSPACE => Key::Backspace,
        SDLK_DELETE => Key::Delete,
        SDLK_RETURN | SDLK_KP_ENTER => Key::Enter,
        SDLK_TAB => Key::Tab,
        SDLK_ESCAPE => Key::Escape,
        SDLK_LEFT => Key::ArrowLeft,
        SDLK_RIGHT => Key::ArrowRight,
        SDLK_UP => Key::ArrowUp,
        SDLK_DOWN => Key::ArrowDown,
        SDLK_HOME => Key::Home,
        SDLK_END => Key::End,
        SDLK_A => Key::A,
        SDLK_C => Key::C,
        SDLK_V => Key::V,
        SDLK_X => Key::X,
        SDLK_Z => Key::Z,
        _ => return None,
    })
}

unsafe fn clipboard_text() -> String {
    unsafe {
        let p = SDL_GetClipboardText();
        if p.is_null() {
            return String::new();
        }
        let text = CStr::from_ptr(p).to_string_lossy().into_owned();
        SDL_free(p as *mut c_void);
        text
    }
}

unsafe fn set_clipboard(text: &str) {
    if let Ok(c) = CString::new(text) {
        unsafe { SDL_SetClipboardText(c.as_ptr()) };
    }
}

/// Where a point of the window (in SDL's units) is on the Mac's screen, 0..32767 each way. None off the
/// picture, unless a button is held: a drag that leaves it goes on along its edge.
unsafe fn on_mac(window: *mut SDL_Window, video: &video::Video, x: f32, y: f32, drag: bool) -> Option<(i32, i32)> {
    unsafe {
        let ((pw, ph), _) = metrics(window);
        let (mut w, mut h) = (0, 0);
        SDL_GetWindowSize(window, &mut w, &mut h);
        let [left, top, width, height] = video.fit((pw, ph));
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let (u, v) = ((x * pw as f32 / w.max(1) as f32 - left) / width, (y * ph as f32 / h.max(1) as f32 - top) / height);
        let inside = (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v);
        (inside || drag).then(|| ((u.clamp(0.0, 1.0) * 32767.0) as i32, (v.clamp(0.0, 1.0) * 32767.0) as i32))
    }
}

/// Lets go, on the Mac, of every key and button it was told is down.
fn release(s: &Session, keys: &mut HashSet<u8>, buttons: &mut HashSet<u8>) {
    keys.drain().for_each(|k| s.send(4, k, 0, 0));
    buttons.drain().for_each(|b| s.send(2, b, 0, 0));
}

/// A controller's state as the phone wants it: the buttons as bits in SDL's order, then six axes. The Mac's
/// controller has 8 bits to an axis, so the lower 8 are dropped here: a stick at rest then sends nothing.
unsafe fn pad_state(pad: *mut SDL_Gamepad) -> [u8; 16] {
    unsafe {
        let mut buttons = 0u32;
        for b in 0..15 {
            buttons |= (SDL_GetGamepadButton(pad, SDL_GamepadButton(b)) as u32) << b;
        }
        let mut out = [0u8; 16];
        out[..4].copy_from_slice(&buttons.to_le_bytes());
        for a in 0..6 {
            out[4 + a * 2..6 + a * 2].copy_from_slice(&(SDL_GetGamepadAxis(pad, SDL_GamepadAxis(a as i32)) & !0xff).to_le_bytes());
        }
        out
    }
}

/// A menu entry of the tray icon was chosen: `userdata` is the event to send the window's loop, type and code.
unsafe extern "C" fn tray_chosen(userdata: *mut c_void, _entry: *mut SDL_TrayEntry) {
    unsafe {
        let mut e: SDL_Event = std::mem::zeroed();
        e.user.r#type = (userdata as usize >> 8) as u32;
        e.user.code = (userdata as usize & 0xff) as i32;
        SDL_PushEvent(&mut e);
    }
}

/// An icon by the clock with Open and Quit, for when the app waits without a window. Null where the desktop
/// has no place for one; the window then simply stays.
unsafe fn make_tray(wake: u32) -> *mut SDL_Tray {
    unsafe {
        // The app's mark, drawn rather than loaded: each of its two shapes row by row, from where the row
        // enters the shape to where it leaves.
        let icon = SDL_CreateSurface(32, 32, SDL_PIXELFORMAT_RGBA32);
        if icon.is_null() {
            return std::ptr::null_mut();
        }
        for (shape, grey) in ui::MARK.iter().zip([0xe2u8, 0xa9]) {
            let ink = SDL_MapSurfaceRGBA(icon, grey, grey, grey, 0xff);
            let at = |(x, y): (f32, f32)| ((x - 230.0) * 0.05 + 1.75, (y - 262.0) * 0.05 + 2.8); // 570 x 528 into 32 x 32
            for row in 0..32 {
                let y = row as f32 + 0.5;
                let (mut left, mut right) = (f32::MAX, f32::MIN);
                for i in 0..shape.len() {
                    let ((x0, y0), (x1, y1)) = (at(shape[i]), at(shape[(i + 1) % shape.len()]));
                    if (y0 <= y) != (y1 <= y) {
                        let x = x0 + (x1 - x0) * (y - y0) / (y1 - y0);
                        (left, right) = (left.min(x), right.max(x));
                    }
                }
                if right > left {
                    SDL_FillSurfaceRect(icon, &SDL_Rect { x: left.round() as i32, y: row, w: (right - left).round().max(1.0) as i32, h: 1 }, ink);
                }
            }
        }
        let tray = SDL_CreateTray(icon, c"Veyron Remote".as_ptr());
        SDL_DestroySurface(icon);
        if tray.is_null() {
            return tray;
        }
        let menu = SDL_CreateTrayMenu(tray);
        for (label, code) in [(c"Open Veyron Remote", 1usize), (c"Quit", 2)] {
            let entry = SDL_InsertTrayEntryAt(menu, -1, label.as_ptr(), SDL_TRAYENTRY_BUTTON);
            SDL_SetTrayEntryCallback(entry, Some(tray_chosen), ((wake as usize) << 8 | code) as *mut c_void);
        }
        tray
    }
}

struct Live {
    session: Arc<Session>,
    target: Target,
    title: String,
    /// The phone answered at least once: a drop after that is made good by connecting again.
    connected: bool,
    /// The user did not ask for this connection, so its failing is not reported.
    quiet: bool,
}

unsafe fn run(background: bool, direct: Option<(String, String)>) {
    unsafe {
        SDL_SetAppMetadata(c"Veyron Remote".as_ptr(), c"0.1.0".as_ptr(), c"veyron-remote".as_ptr());
        SDL_SetHint(SDL_HINT_AUDIO_DEVICE_SAMPLE_FRAMES, c"256".as_ptr());
        SDL_SetHint(SDL_HINT_MOUSE_FOCUS_CLICKTHROUGH, c"1".as_ptr());
        if !SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO | SDL_INIT_GAMEPAD) {
            eprintln!("veyron-remote: SDL could not start: {}", CStr::from_ptr(SDL_GetError()).to_string_lossy());
            std::process::exit(1);
        }
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 3);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_CORE.0 as i32);
        let mut flags = SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY;
        if background {
            flags |= SDL_WINDOW_HIDDEN;
        }
        let window = SDL_CreateWindow(c"Veyron Remote".as_ptr(), 1040, 680, flags);
        let context = if window.is_null() { std::ptr::null_mut() } else { SDL_GL_CreateContext(window) };
        if context.is_null() {
            eprintln!("veyron-remote: no OpenGL 3.3 window: {}", CStr::from_ptr(SDL_GetError()).to_string_lossy());
            std::process::exit(1);
        }
        SDL_SetWindowMinimumSize(window, 520, 420);
        // Pictures are shown the moment they are decoded; waiting for the display's refresh would add up to a frame.
        SDL_GL_SetSwapInterval(0);
        let gl = Arc::new(glow::Context::from_loader_function_cstr(|name| SDL_GL_GetProcAddress(name.as_ptr()).map_or(std::ptr::null(), |f| f as *const c_void)));
        let mut painter = egui_glow::Painter::new(gl.clone(), "", None, false).expect("egui painter");
        let mut video = video::Video::new(&gl);
        let ctx = egui::Context::default();
        ui::theme(&ctx);

        let wake = SDL_RegisterEvents(1);
        let poke = move || {
            let mut e: SDL_Event = std::mem::zeroed();
            e.user.r#type = wake;
            SDL_PushEvent(&mut e);
        };
        let found = Discovery::start(poke);
        let mut cfg = Config::load();
        // Waiting for a Mac without a window needs somewhere to be reached from.
        let tray = if background || cfg.auto_connect || cfg.start_at_login { make_tray(wake) } else { std::ptr::null_mut() };
        let mut st = ui::State::default();
        let started = Instant::now();
        let mut live: Option<Live> = None;
        let mut retry: Option<(Instant, Target, String)> = None;
        let mut hold_auto = false; // the user disconnected: do not connect again by itself until the phone is replugged
        let mut tried_auto = Instant::now() - Duration::from_secs(60);
        let mut captured = false;
        // The seamless mouse: whether the pointer is on the Mac's picture (which has the mouse then), where
        // on the Mac's screen it is, which buttons the Mac holds, and whether it has yet to be told.
        let mut over_mac = false;
        let (mut abs, mut abs_buttons, mut abs_dirty) = ((0i32, 0i32), 0u8, false);
        let (mut grabbed, mut cursor_hidden) = (false, false);
        let mut fullscreen = false;
        let mut hidden = background && !tray.is_null();
        if background && tray.is_null() {
            // Nowhere to put an icon (on Linux SDL wants libayatana-appindicator for it): wait in the taskbar instead.
            SDL_ShowWindow(window);
            SDL_MinimizeWindow(window);
        }
        let mut focused = true;
        let (mut held_keys, mut held_buttons) = (HashSet::<u8>::new(), HashSet::<u8>::new());
        let (mut move_x, mut move_y, mut wheel_x, mut wheel_y) = (0f32, 0f32, 0f32, 0f32);
        let mut pads: HashMap<u32, *mut SDL_Gamepad> = HashMap::new();
        let mut pad_sent = [0u8; 16];
        let mut pad_live = false; // the controller is plugged into the Mac: from its first press until the connection ends
        let mut clip_seen = String::new();
        let mut events: Vec<egui::Event> = Vec::new();
        let mut cursors: HashMap<u32, *mut SDL_Cursor> = HashMap::new();
        let mut typing = false;
        let mut drawn = Instant::now() - Duration::from_secs(1);
        let mut dirty = true;
        let mut wait = Duration::from_millis(100);
        let mut repaint_at: Option<Instant> = None; // when egui wants to be drawn again (an animation, a tooltip)

        if let Some((address, code)) = direct {
            let (host, port) = address.rsplit_once(':').map_or((address.clone(), session::PORT), |(h, p)| (h.to_string(), p.parse().unwrap_or(session::PORT)));
            retry = Some((Instant::now(), Target::Net { host: host.clone(), port, code }, host));
        }

        'app: loop {
            // ---- what the system has to say ----
            let mut ev: SDL_Event = std::mem::zeroed();
            let mut have = SDL_WaitEventTimeout(&mut ev, wait.as_millis().clamp(1, 500) as i32);
            // The mouse goes to the Mac by position: the setting, a Veyron that can, and a picture to point at.
            let seamless_mode = cfg.seamless && live.as_ref().is_some_and(|l| l.session.info().is_some_and(|i| i.pointer));
            let seamless = seamless_mode && !captured && !st.settings && retry.is_none() && video.width > 0 && live.as_ref().is_some_and(|l| l.session.phase().0 == Phase::Live);
            while have {
                let session = live.as_ref().map(|l| l.session.clone());
                let kind = SDL_EventType(ev.r#type);
                match kind {
                    SDL_EVENT_QUIT => break 'app,
                    // With the tray icon there, closing the window leaves the app waiting for a Mac.
                    SDL_EVENT_WINDOW_CLOSE_REQUESTED if tray.is_null() || live.is_some() => break 'app,
                    SDL_EVENT_WINDOW_CLOSE_REQUESTED => {
                        hidden = true;
                        SDL_HideWindow(window);
                    }
                    _ if ev.r#type == wake && ev.user.code == 2 => break 'app,
                    _ if ev.r#type == wake && ev.user.code == 1 => {
                        (hidden, dirty) = (false, true);
                        SDL_ShowWindow(window);
                        SDL_RaiseWindow(window);
                    }
                    SDL_EVENT_MOUSE_MOTION if captured => {
                        move_x += ev.motion.xrel * cfg.pointer_speed;
                        move_y += ev.motion.yrel * cfg.pointer_speed;
                    }
                    SDL_EVENT_MOUSE_MOTION => {
                        let p = to_points(window, ev.motion.x, ev.motion.y);
                        let own = abs_buttons == 0 && st.ui_rect.is_some_and(|r| r.contains(p)); // on the bar: the app's
                        match on_mac(window, &video, ev.motion.x, ev.motion.y, abs_buttons != 0).filter(|_| seamless && !own) {
                            Some(at) => {
                                if !over_mac {
                                    events.push(egui::Event::PointerGone);
                                    dirty = true;
                                }
                                (over_mac, abs_dirty, abs) = (true, abs_dirty || at != abs, at);
                            }
                            None => {
                                over_mac = false;
                                events.push(egui::Event::PointerMoved(p));
                                st.moved = started.elapsed().as_secs_f64();
                                dirty = true;
                            }
                        }
                    }
                    SDL_EVENT_MOUSE_BUTTON_DOWN | SDL_EVENT_MOUSE_BUTTON_UP if captured => {
                        // SDL: 1 left, 2 middle, 3 right. Veyron: 0 left, 1 right, 2 middle.
                        let button = match ev.button.button { 1 => 0, 3 => 1, 2 => 2, _ => 255 };
                        if let (Some(s), true) = (&session, button != 255) {
                            flush_move(s, &mut move_x, &mut move_y); // a click lands where the pointer is by then
                            s.send(2, button, ev.button.down as i32, 0);
                            if ev.button.down { held_buttons.insert(button) } else { held_buttons.remove(&button) };
                        }
                    }
                    SDL_EVENT_MOUSE_BUTTON_DOWN | SDL_EVENT_MOUSE_BUTTON_UP => {
                        let p = to_points(window, ev.button.x, ev.button.y);
                        let bit = match ev.button.button { 1 => 1u8, 3 => 2, 2 => 4, _ => 0 }; // the Mac's: left, right, middle
                        // A press is the Mac's when it lands on its picture and not on the bar; a release goes where its press went.
                        let mac = if ev.button.down {
                            seamless && bit != 0 && !st.ui_rect.is_some_and(|r| r.contains(p)) && on_mac(window, &video, ev.button.x, ev.button.y, false).is_some()
                        } else {
                            abs_buttons & bit != 0
                        };
                        if let (Some(s), true) = (&session, mac) {
                            abs_buttons = if ev.button.down { abs_buttons | bit } else { abs_buttons & !bit };
                            abs = on_mac(window, &video, ev.button.x, ev.button.y, true).unwrap_or(abs);
                            s.send(11, abs_buttons, abs.0, abs.1);
                            (over_mac, abs_dirty) = (true, false);
                        } else {
                            let button = match ev.button.button { 1 => Some(egui::PointerButton::Primary), 2 => Some(egui::PointerButton::Middle), 3 => Some(egui::PointerButton::Secondary), _ => None };
                            if let Some(button) = button {
                                events.push(egui::Event::PointerButton { pos: p, button, pressed: ev.button.down, modifiers: modifiers(SDL_GetModState()) });
                            }
                            dirty = true;
                        }
                    }
                    SDL_EVENT_MOUSE_WHEEL if captured || (seamless && over_mac) => {
                        wheel_x += ev.wheel.x;
                        wheel_y += ev.wheel.y;
                        let (x, y) = (wheel_x.trunc(), wheel_y.trunc());
                        if let (Some(s), true) = (&session, x != 0.0 || y != 0.0) {
                            flush_move(s, &mut move_x, &mut move_y); // it scrolls what the pointer is on by then
                            if abs_dirty {
                                s.send(11, abs_buttons, abs.0, abs.1);
                                abs_dirty = false;
                            }
                            s.send(3, 0, x as i32, y as i32);
                            wheel_x -= x;
                            wheel_y -= y;
                        }
                    }
                    SDL_EVENT_MOUSE_WHEEL => {
                        events.push(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(ev.wheel.x, ev.wheel.y), phase: egui::TouchPhase::Move, modifiers: modifiers(SDL_GetModState()) });
                        dirty = true;
                    }
                    SDL_EVENT_KEY_DOWN | SDL_EVENT_KEY_UP if captured || seamless => {
                        let both = SDL_KMOD_LCTRL.0 | SDL_KMOD_LALT.0;
                        if ev.key.down && !ev.key.repeat && ev.key.r#mod.0 & both == both && matches!(ev.key.scancode, SDL_SCANCODE_LCTRL | SDL_SCANCODE_LALT) {
                            // Left Ctrl + Left Alt: the Mac lets go of a mouse it holds. With the seamless mouse it
                            // holds none, and this hands it over instead, for a game that turns with the mouse.
                            captured = !captured;
                            if let Some(s) = &session {
                                release(s, &mut held_keys, &mut held_buttons); // the Ctrl of the pair is down in the Mac
                            }
                            dirty = true;
                        } else if seamless && ev.key.key == SDLK_F11 {
                            if ev.key.down && !ev.key.repeat {
                                fullscreen = !fullscreen;
                                SDL_SetWindowFullscreen(window, fullscreen);
                            }
                        } else if let (Some(s), false) = (&session, ev.key.repeat) {
                            // SDL's scancodes are USB HID usages, which is what the Mac's keyboard speaks.
                            let mut usage = ev.key.scancode.0;
                            if cfg.alt_is_command {
                                usage = match usage { 226 => 227, 227 => 226, 230 => 231, 231 => 230, u => u };
                            }
                            if (1..256).contains(&usage) {
                                s.send(4, usage as u8, ev.key.down as i32, 0);
                                if ev.key.down { held_keys.insert(usage as u8) } else { held_keys.remove(&(usage as u8)) };
                            }
                        }
                    }
                    SDL_EVENT_KEY_DOWN | SDL_EVENT_KEY_UP => {
                        let mods = modifiers(ev.key.r#mod);
                        if ev.key.down && ev.key.key == SDLK_F11 && live.is_some() {
                            fullscreen = !fullscreen;
                            SDL_SetWindowFullscreen(window, fullscreen);
                        } else if ev.key.down && mods.command && ev.key.key == SDLK_V {
                            events.push(egui::Event::Paste(clipboard_text()));
                        } else if ev.key.down && mods.command && ev.key.key == SDLK_C {
                            events.push(egui::Event::Copy);
                        } else if ev.key.down && mods.command && ev.key.key == SDLK_X {
                            events.push(egui::Event::Cut);
                        } else if let Some(key) = egui_key(ev.key.key) {
                            events.push(egui::Event::Key { key, physical_key: None, pressed: ev.key.down, repeat: ev.key.repeat, modifiers: mods });
                        }
                        dirty = true;
                    }
                    SDL_EVENT_TEXT_INPUT if !captured => {
                        events.push(egui::Event::Text(CStr::from_ptr(ev.text.text).to_string_lossy().into_owned()));
                        dirty = true;
                    }
                    SDL_EVENT_WINDOW_FOCUS_LOST => {
                        (focused, captured, dirty) = (false, false, true);
                        if let Some(s) = &session {
                            release(s, &mut held_keys, &mut held_buttons); // their releases will go to another window
                        }
                        events.push(egui::Event::WindowFocused(false));
                    }
                    SDL_EVENT_WINDOW_FOCUS_GAINED => {
                        (focused, dirty) = (true, true);
                        events.push(egui::Event::WindowFocused(true));
                    }
                    SDL_EVENT_WINDOW_MOUSE_LEAVE => {
                        over_mac &= abs_buttons != 0; // a drag goes on outside the window
                        events.push(egui::Event::PointerGone);
                        dirty = true;
                    }
                    SDL_EVENT_GAMEPAD_ADDED => {
                        let pad = SDL_OpenGamepad(ev.gdevice.which);
                        if !pad.is_null() {
                            pads.insert(ev.gdevice.which.0, pad);
                        }
                    }
                    SDL_EVENT_GAMEPAD_REMOVED => {
                        if let Some(pad) = pads.remove(&ev.gdevice.which.0) {
                            SDL_CloseGamepad(pad);
                        }
                    }
                    SDL_EVENT_CLIPBOARD_UPDATE => {
                        if let (Some(s), true) = (&session, cfg.clipboard && focused) {
                            let text = clipboard_text();
                            if !text.is_empty() && text != clip_seen {
                                s.send_long(10, 0, text.as_bytes());
                                clip_seen = text;
                            }
                        }
                    }
                    _ if (0x600..0x700).contains(&ev.r#type) => {} // sticks and buttons: read as a whole further down
                    _ => dirty = true, // the window changed, or another thread has something to show
                }
                have = SDL_PollEvent(&mut ev);
            }

            // ---- the connection ----
            let now = Instant::now();
            if let Some(l) = &mut live {
                let (phase, reason) = l.session.phase();
                l.connected |= matches!(phase, Phase::Waiting | Phase::Live);
                if phase == Phase::Ended {
                    captured = false;
                    if l.connected {
                        retry = Some((now + Duration::from_millis(1200), l.target.clone(), l.title.clone()));
                    } else {
                        if !l.quiet {
                            st.error = Some((reason, started.elapsed().as_secs_f64()));
                        }
                        video.clear();
                        if fullscreen {
                            fullscreen = false;
                            SDL_SetWindowFullscreen(window, false);
                        }
                    }
                    live = None;
                    dirty = true;
                }
            }
            let usb = found.usb.lock().unwrap().clone();
            if usb.is_empty() {
                hold_auto = false;
            }
            let mut connect: Option<(Target, String, bool)> = None;
            if let Some((at, target, title)) = &retry {
                if live.is_none() && now >= *at {
                    connect = Some((target.clone(), title.clone(), false));
                }
            } else if live.is_none() && cfg.auto_connect && !hold_auto && now.duration_since(tried_auto) > Duration::from_secs(4) {
                // A Veyron that says whether a Mac runs is only tried when one does; an older one is simply tried.
                if let Some(f) = usb.iter().find(|f| f.info.as_ref().is_none_or(|i| i.running)) {
                    tried_auto = now;
                    connect = Some((f.target.clone(), f.title.clone(), true));
                }
            }

            // ---- what the Mac gets ----
            if let Some(l) = &live {
                let s = &l.session;
                if captured {
                    flush_move(s, &mut move_x, &mut move_y);
                } else if !seamless {
                    // The mouse came back with keys still down: let them go on the Mac, or they repeat forever.
                    release(s, &mut held_keys, &mut held_buttons);
                }
                if !seamless {
                    if abs_buttons != 0 {
                        s.send(11, 0, abs.0, abs.1); // the same for a button the seamless mouse held
                    }
                    (over_mac, abs_buttons, abs_dirty) = (false, 0, false);
                } else if abs_dirty {
                    s.send(11, abs_buttons, abs.0, abs.1);
                    abs_dirty = false;
                }
                if cfg.controller && s.framed() {
                    if let Some(pad) = pads.values().next() {
                        let state = pad_state(*pad);
                        // It appears in the Mac with the first button or trigger pressed, not for merely being
                        // attached to this PC (a handheld's own controller always is).
                        pad_live |= state[..4] != [0; 4] || state[13] > 0x20 || state[15] > 0x20;
                        if pad_live && state != pad_sent {
                            s.send_long(9, 0, &state);
                            pad_sent = state;
                        }
                    }
                }
                if let Some(text) = s.take_clipboard() {
                    if cfg.clipboard && text != clip_seen {
                        set_clipboard(&text);
                        clip_seen = text;
                    }
                }
                s.set_gain(cfg.volume);
            } else {
                (held_keys.clear(), held_buttons.clear());
                (over_mac, abs_buttons, abs_dirty) = (false, 0, false);
                (pad_live, pad_sent) = (false, [0; 16]);
            }
            let relative = SDL_GetWindowRelativeMouseMode(window);
            if relative != captured {
                SDL_SetWindowRelativeMouseMode(window, captured);
                st.captured_at = started.elapsed().as_secs_f64();
                (move_x, move_y) = (0.0, 0.0);
            }
            // While the Mac has the mouse, the desktop's own shortcuts (Super, Alt+Tab) go to it as well, and
            // its pointer in the picture is the only one shown.
            let on_picture = seamless && over_mac;
            if grabbed != (captured || (on_picture && focused)) {
                grabbed = !grabbed;
                SDL_SetWindowKeyboardGrab(window, grabbed);
            }
            if cursor_hidden != on_picture {
                cursor_hidden = on_picture;
                if cursor_hidden { SDL_HideCursor() } else { SDL_ShowCursor() };
            }

            // ---- drawing: at once for a new picture, at most every 7 ms for anything else ----
            let fresh = live.as_ref().is_some_and(|l| l.session.frame.lock().unwrap().serial != video.serial);
            dirty |= repaint_at.is_some_and(|at| now >= at);
            if hidden || !(fresh || dirty) || (!fresh && drawn.elapsed() < Duration::from_millis(7)) {
                if hidden && live.as_ref().is_some_and(|l| l.connected) {
                    (hidden, dirty) = (false, true); // a Mac appeared while waiting in the background
                    SDL_ShowWindow(window);
                    SDL_RaiseWindow(window);
                }
                wait = if hidden {
                    Duration::from_millis(250) // nothing to draw: only the connection to look after
                } else if dirty {
                    Duration::from_millis(7).saturating_sub(drawn.elapsed())
                } else {
                    repaint_at.map_or(Duration::from_millis(500), |at| at.saturating_duration_since(now))
                };
                if let Some((target, title, quiet)) = connect {
                    live = Some(open(&cfg, wake, target, title, quiet, &mut retry, &found, &mut video));
                }
                continue;
            }
            let ((pw, ph), ppp) = metrics(window);
            gl.viewport(0, 0, pw, ph);
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            if let Some(l) = &live {
                video.upload(&gl, &l.session.frame.lock().unwrap());
            }
            if live.is_some() || retry.is_some() {
                video.draw(&gl, (pw, ph));
            }

            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(pw as f32, ph as f32) / ppp)),
                time: Some(started.elapsed().as_secs_f64()),
                events: if captured { Vec::new() } else { std::mem::take(&mut events) },
                focused,
                ..Default::default()
            };
            raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
            raw.events.insert(0, egui::Event::ModifiersChanged(modifiers(SDL_GetModState())));
            if captured {
                events.clear();
                raw.events.push(egui::Event::PointerGone);
            }
            let before = cfg.clone();
            let mut actions = Vec::new();
            let mut out = ctx.run_ui(raw, |ui| {
                let protocol2 = live.as_ref().filter(|l| l.session.phase().0 == Phase::Live).map(|l| l.session.framed());
                match (&live, &retry) {
                    (None, None) => ui::home(ui, &mut st, &mut cfg, &found.all(), found.no_usbmuxd.load(Ordering::Relaxed), &mut actions),
                    _ => {
                        let stats = live.as_ref().map(|l| l.session.stats()).unwrap_or_default();
                        // The Mac's own name when the phone tells it; otherwise the phone's, or the address.
                        let mac = live.as_ref().and_then(|l| l.session.info()).map(|i| i.name).filter(|n| !n.is_empty());
                        let view = ui::Stream {
                            title: mac.as_deref().or(live.as_ref().map(|l| l.title.as_str())).or(retry.as_ref().map(|r| r.2.as_str())).unwrap_or(""),
                            phase: live.as_ref().map_or(Phase::Connecting, |l| l.session.phase().0),
                            retrying: retry.is_some(),
                            has_picture: video.width > 0,
                            captured,
                            seamless: seamless_mode,
                            fullscreen,
                            stats: &stats,
                        };
                        ui::stream(ui, &mut st, &mut cfg, &view, &mut actions);
                    }
                }
                ui::settings(ui.ctx(), &mut st, &mut cfg, protocol2);
            });

            for command in out.platform_output.commands.drain(..) {
                if let egui::OutputCommand::CopyText(text) = command {
                    set_clipboard(&text);
                }
            }
            let wants_text = ctx.egui_wants_keyboard_input();
            if wants_text != typing {
                typing = wants_text;
                if typing { SDL_StartTextInput(window) } else { SDL_StopTextInput(window) };
            }
            let shape = match out.platform_output.cursor_icon {
                egui::CursorIcon::PointingHand => SDL_SYSTEM_CURSOR_POINTER,
                egui::CursorIcon::Text => SDL_SYSTEM_CURSOR_TEXT,
                _ => SDL_SYSTEM_CURSOR_DEFAULT,
            };
            SDL_SetCursor(*cursors.entry(shape.0 as u32).or_insert_with(|| SDL_CreateSystemCursor(shape)));
            let shapes = std::mem::take(&mut out.shapes);
            let primitives = ctx.tessellate(shapes, out.pixels_per_point);
            painter.paint_and_update_textures([pw as u32, ph as u32], out.pixels_per_point, &primitives, &mut out.textures_delta);
            SDL_GL_SwapWindow(window);
            drawn = Instant::now();
            dirty = false;
            let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
            repaint_at = drawn.checked_add(delay);
            wait = delay.clamp(Duration::from_millis(7), Duration::from_millis(if retry.is_some() { 100 } else { 500 }));

            // ---- what the user asked for ----
            if cfg != before {
                cfg.save();
                if cfg.start_at_login != before.start_at_login {
                    config::set_start_at_login(cfg.start_at_login);
                }
                if let (Some(l), true) = (&live, cfg.tuning != before.tuning) {
                    l.session.tune(&cfg.tuning);
                }
            }
            for action in actions {
                match action {
                    ui::Action::Connect(target, title) => {
                        if let Target::Net { host, port, code } = &target {
                            cfg.remember(host, *port, code);
                            cfg.save();
                        }
                        connect = Some((target, title, false));
                    }
                    ui::Action::Disconnect => {
                        if let Some(l) = live.take() {
                            l.session.close();
                        }
                        (retry, captured, hold_auto) = (None, false, true);
                        video.clear();
                        found.paused.store(false, Ordering::Relaxed);
                        if fullscreen {
                            fullscreen = false;
                            SDL_SetWindowFullscreen(window, false);
                        }
                    }
                    ui::Action::Capture => captured = true,
                    ui::Action::Fullscreen => {
                        fullscreen = !fullscreen;
                        SDL_SetWindowFullscreen(window, fullscreen);
                    }
                }
                dirty = true;
            }
            if let Some((target, title, quiet)) = connect {
                live = Some(open(&cfg, wake, target, title, quiet, &mut retry, &found, &mut video));
                if cfg.fullscreen_on_connect && !fullscreen && !quiet {
                    fullscreen = true;
                    SDL_SetWindowFullscreen(window, true);
                }
                dirty = true;
            }
        }

        if let Some(l) = live.take() {
            l.session.close();
        }
        cfg.save();
        painter.destroy();
        if !tray.is_null() {
            SDL_DestroyTray(tray);
        }
        SDL_GL_DestroyContext(context);
        SDL_DestroyWindow(window);
        SDL_Quit();
    }
}

/// Starts a connection. A retry keeps the last picture on screen; anything else starts from black.
fn open(cfg: &Config, wake: u32, target: Target, title: String, quiet: bool, retry: &mut Option<(Instant, Target, String)>, found: &Discovery, video: &mut video::Video) -> Live {
    let again = retry.take().is_some();
    if !again {
        video.clear();
    }
    found.paused.store(true, Ordering::Relaxed);
    let options = Options { hardware: cfg.hardware, sound: cfg.sound, tuning: cfg.tuning, name: this_computer(), wake };
    Live { session: Session::open(target.clone(), options), target, title, connected: again, quiet }
}

/// Mouse movement adds up between passes of the loop and goes to the Mac as one message.
fn flush_move(s: &Session, x: &mut f32, y: &mut f32) {
    let (dx, dy) = (x.trunc(), y.trunc());
    if dx != 0.0 || dy != 0.0 {
        s.send(1, 0, dx as i32, dy as i32);
        *x -= dx;
        *y -= dy;
    }
}

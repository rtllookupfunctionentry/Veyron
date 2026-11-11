//! Everything drawn over or instead of the Mac's picture: the list of Macs to connect to, the bar and
//! messages over a running stream, and the settings sheet. Dark and quiet: near-black, one warm accent.
use crate::config::Config;
use crate::discover::Found;
use crate::session::{Phase, Stats, Target};
use egui::{Align, Align2, Area, Color32, Context, CursorIcon, FontFamily, FontId, Frame, Id, Layout, Order, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};
use egui_phosphor::regular as icon;

const BG: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
const PANEL: Color32 = Color32::from_rgb(0x13, 0x13, 0x16);
const RAISED: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x20);
const HOVER: Color32 = Color32::from_rgb(0x24, 0x24, 0x2a);
const LINE: Color32 = Color32::from_rgb(0x27, 0x27, 0x2d);
const TEXT: Color32 = Color32::from_rgb(0xed, 0xed, 0xf0);
const MUTED: Color32 = Color32::from_rgb(0x8e, 0x8e, 0x99);
const FAINT: Color32 = Color32::from_rgb(0x5c, 0x5c, 0x66);
const ACCENT: Color32 = Color32::from_rgb(0xf2, 0xa6, 0x5a);
const ACCENT_HI: Color32 = Color32::from_rgb(0xf8, 0xbd, 0x80);
const ON_ACCENT: Color32 = Color32::from_rgb(0x1c, 0x12, 0x06);
const GOOD: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
const BAD: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);

pub enum Action {
    Connect(Target, String),
    Disconnect,
    /// Hand the mouse and keyboard to the Mac.
    Capture,
    Fullscreen,
}

#[derive(Default)]
pub struct State {
    pub settings: bool,
    pub host: String,
    pub code: String,
    /// A Mac found on Wi-Fi was chosen and its code is being asked for.
    pub prompt: Option<Found>,
    pub prompt_code: String,
    /// What went wrong last, and when (egui's clock) it was put up.
    pub error: Option<(String, f64)>,
    /// When the pointer last moved over the stream: the bar hides a moment after.
    pub moved: f64,
    pub captured_at: f64,
    /// With the seamless mouse: the part of the stream that is the app's own (the bar and the grip it opens
    /// from). The mouse stays with the app there and goes to the Mac everywhere else on the picture.
    pub ui_rect: Option<Rect>,
}

fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Icons have a family of their own: Inter draws letters of its own at some of the icon font's code points.
fn icons(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semi".into()))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), f(a.a(), b.a()))
}

pub fn theme(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("inter".into(), egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.ttf")).into());
    fonts.font_data.insert("inter-semi".into(), egui::FontData::from_static(include_bytes!("../assets/Inter-SemiBold.ttf")).into());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    let regular = fonts.families.entry(FontFamily::Proportional).or_default();
    regular.insert(0, "inter".into());
    let mut bold = regular.clone();
    bold[0] = "inter-semi".into();
    fonts.families.insert(FontFamily::Name("semi".into()), bold);
    fonts.families.insert(FontFamily::Name("icons".into()), vec!["phosphor".into()]);
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        s.visuals = egui::Visuals::dark();
        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.extreme_bg_color = Color32::from_rgb(0x0e, 0x0e, 0x11);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        for (w, fill) in [(&mut v.widgets.inactive, RAISED), (&mut v.widgets.hovered, HOVER), (&mut v.widgets.active, HOVER), (&mut v.widgets.open, HOVER)] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = Stroke::new(1.0, LINE);
            w.corner_radius = 8.into();
            w.fg_stroke = Stroke::new(1.0, TEXT);
            w.expansion = 0.0;
        }
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, FAINT);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(10.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.slider_width = 150.0;
        s.spacing.slider_rail_height = 4.0;
        s.visuals.slider_trailing_fill = true;
        s.visuals.handle_shape = egui::style::HandleShape::Circle;
        s.spacing.scroll.floating = true;
        for (style, font) in s.text_styles.iter_mut() {
            font.size = match style {
                egui::TextStyle::Heading => 20.0,
                egui::TextStyle::Small => 11.5,
                egui::TextStyle::Monospace => 12.0,
                _ => 13.5,
            };
        }
    });
}

// ---- small parts ----

fn button(ui: &mut Ui, text: &str, primary: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), semi(13.0), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 32.0), Sense::click());
    let t = ui.ctx().animate_bool_responsive(resp.id.with("hover"), resp.hovered());
    let (fill, ink) = if primary { (mix(ACCENT, ACCENT_HI, t), ON_ACCENT) } else { (mix(RAISED, HOVER, t), TEXT) };
    ui.painter().rect_filled(rect, 9.0, fill);
    if !primary {
        ui.painter().rect_stroke(rect, 9.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn icon_button(ui: &mut Ui, glyph: &str, tip: &str, tint: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let t = ui.ctx().animate_bool_responsive(resp.id.with("hover"), resp.hovered());
    ui.painter().rect_filled(rect, 8.0, HOVER.gamma_multiply(t));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, icons(17.0), mix(MUTED, tint, t));
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip)
}

fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    ui.painter().rect_filled(rect, 10.0, mix(HOVER, ACCENT, t));
    let x = rect.left() + 10.0 + (rect.width() - 20.0) * t;
    ui.painter().circle_filled(pos2(x, rect.center().y), 7.0, mix(TEXT, ON_ACCENT, t));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) {
    Frame::new().fill(Color32::from_rgb(0x0e, 0x0e, 0x11)).corner_radius(8).inner_margin(2).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // A row lays its control out from the right, which would put the options in reverse.
            let ordered: Vec<_> = if ui.layout().prefer_right_to_left() { options.iter().rev().collect() } else { options.iter().collect() };
            for (option, label) in ordered {
                let on = *value == *option;
                let galley = ui.painter().layout_no_wrap(label.to_string(), body(12.5), Color32::PLACEHOLDER);
                let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 18.0, 24.0), Sense::click());
                let t = ui.ctx().animate_bool_responsive(resp.id, on);
                ui.painter().rect_filled(rect, 6.0, HOVER.gamma_multiply(t));
                ui.painter().galley(rect.center() - galley.size() / 2.0, galley, mix(MUTED, TEXT, t.max(if resp.hovered() { 0.6 } else { 0.0 })));
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    *value = *option;
                }
            }
        });
    });
}

fn caps(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(semi(11.0)).color(FAINT).extra_letter_spacing(0.8));
}

/// One line of the settings sheet: what it is and, smaller, what it does; the control on the right.
fn row(ui: &mut Ui, title: &str, hint: &str, enabled: bool, control: impl FnOnce(&mut Ui)) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_max_width(ui.available_width() - 150.0);
                ui.label(RichText::new(title).size(13.5));
                if !hint.is_empty() {
                    ui.label(RichText::new(hint).size(11.5).color(MUTED));
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
    });
    ui.add_space(6.0);
}

/// A card in the list of Macs: a tile with an icon, two lines, and what clicking does on the right.
fn card(ui: &mut Ui, glyph: &str, title: &str, detail: &str, dot: Option<Color32>, action: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 68.0), Sense::click());
    let t = ui.ctx().animate_bool_responsive(resp.id.with("hover"), resp.hovered());
    let p = ui.painter();
    p.rect_filled(rect, 14.0, mix(PANEL, RAISED, t));
    p.rect_stroke(rect, 14.0, Stroke::new(1.0, mix(LINE, FAINT, t)), StrokeKind::Inside);
    let tile = Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(40.0, 40.0));
    p.rect_filled(tile, 10.0, mix(RAISED, HOVER, t));
    p.text(tile.center(), Align2::CENTER_CENTER, glyph, icons(20.0), mix(TEXT, ACCENT, t));
    p.text(pos2(tile.right() + 14.0, rect.top() + 15.0), Align2::LEFT_TOP, title, semi(14.5), TEXT);
    let mut x = tile.right() + 14.0;
    if let Some(dot) = dot {
        p.circle_filled(pos2(x + 3.5, rect.top() + 46.0), 3.5, dot);
        x += 13.0;
    }
    p.text(pos2(x, rect.top() + 38.0), Align2::LEFT_TOP, detail, body(12.5), MUTED);
    let label = p.layout_no_wrap(action.to_string(), semi(12.5), Color32::PLACEHOLDER);
    let pill = Rect::from_min_size(pos2(rect.right() - 16.0 - label.size().x - 24.0, rect.center().y - 15.0), vec2(label.size().x + 24.0, 30.0));
    p.rect_filled(pill, 9.0, mix(RAISED, ACCENT, t));
    p.galley(pill.center() - label.size() / 2.0, label, mix(TEXT, ON_ACCENT, t));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(egui::TextEdit::singleline(text).hint_text(RichText::new(hint).color(FAINT)).desired_width(width).margin(vec2(10.0, 7.0)))
}

fn code_only(text: &mut String) {
    text.retain(|c| c.is_ascii_digit());
    text.truncate(6);
}

// ---- the list of Macs ----

pub fn home(ui: &mut Ui, st: &mut State, cfg: &mut Config, found: &[Found], no_usbmuxd: bool, out: &mut Vec<Action>) {
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, BG);
    let p = ui.painter().clone();
    let mark = p.text(pos2(full.left() + 24.0, full.top() + 28.0), Align2::LEFT_CENTER, "Henge", semi(16.0), TEXT);
    p.text(pos2(mark.right() + 6.0, full.top() + 28.0), Align2::LEFT_CENTER, "Remote", body(16.0), MUTED);
    let gear = Rect::from_min_size(pos2(full.right() - 24.0 - 32.0, full.top() + 12.0), vec2(32.0, 32.0));
    ui.scope_builder(UiBuilder::new().max_rect(gear), |ui| {
        if icon_button(ui, icon::SLIDERS_HORIZONTAL, "Settings", TEXT).clicked() {
            st.settings = !st.settings;
        }
    });

    let width = (full.width() - 48.0).min(560.0);
    let column = Rect::from_min_max(pos2(full.center().x - width / 2.0, full.top() + 76.0), pos2(full.center().x + width / 2.0, full.bottom() - 44.0));
    ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            caps(ui, "NEARBY");
            if found.is_empty() {
                empty(ui, no_usbmuxd);
            }
            for f in found {
                let (glyph, way) = if f.usb { (icon::DEVICE_MOBILE, "USB") } else { (icon::WIFI_HIGH, "Wi-Fi") };
                let (detail, dot) = match &f.info {
                    Some(i) if i.running && !i.name.is_empty() => (format!("{way}  ·  {} is running", i.name), Some(GOOD)),
                    Some(i) if i.running => (format!("{way}  ·  A Mac is running"), Some(GOOD)),
                    Some(_) => (format!("{way}  ·  No Mac is running in Henge"), Some(FAINT)),
                    None => (way.to_string(), None),
                };
                if card(ui, glyph, &f.title, &detail, dot, "Connect").clicked() {
                    match &f.target {
                        Target::Net { host, port, .. } => match cfg.saved.iter().find(|s| &s.host == host && s.port == *port) {
                            Some(s) => out.push(Action::Connect(Target::Net { host: host.clone(), port: *port, code: s.code.clone() }, f.title.clone())),
                            None => (st.prompt, st.prompt_code) = (Some(f.clone()), String::new()),
                        },
                        usb => out.push(Action::Connect(usb.clone(), f.title.clone())),
                    }
                }
            }
            ui.add_space(14.0);
            caps(ui, "BY ADDRESS");
            Frame::new().fill(PANEL).stroke(Stroke::new(1.0, LINE)).corner_radius(14).inner_margin(14).show(ui, |ui| {
                ui.horizontal(|ui| {
                    code_only(&mut st.code);
                    let host = field(ui, &mut st.host, "192.168.1.20", ui.available_width() - 214.0);
                    let code = field(ui, &mut st.code, "Code", 70.0);
                    let enter = (host.lost_focus() || code.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (button(ui, "Connect", true).clicked() || enter) && !st.host.trim().is_empty() {
                        let (host, port) = split_address(st.host.trim());
                        out.push(Action::Connect(Target::Net { host: host.clone(), port, code: st.code.clone() }, host));
                    }
                });
                ui.label(RichText::new("The address and the six-digit code are in Henge, under Remote Desktop, once Wi-Fi is allowed there.").size(11.5).color(MUTED));
            });
            let mut forget = None;
            for (i, s) in cfg.saved.iter().enumerate() {
                let resp = card(ui, icon::CLOCK_COUNTER_CLOCKWISE, &s.host, "Connected before  ·  right-click to forget", None, "Connect");
                if resp.clicked() {
                    out.push(Action::Connect(Target::Net { host: s.host.clone(), port: s.port, code: s.code.clone() }, s.host.clone()));
                }
                if resp.secondary_clicked() {
                    forget = Some(i);
                }
            }
            if let Some(i) = forget {
                cfg.saved.remove(i);
            }
        });
    });

    let foot = if no_usbmuxd { "usbmuxd is not running, so phones on a cable cannot be seen." } else { "Looking for iPhones on USB and Wi-Fi" };
    p.text(pos2(full.left() + 24.0, full.bottom() - 22.0), Align2::LEFT_CENTER, foot, body(12.0), FAINT);
    toast(ui.ctx(), st);
    prompt(ui.ctx(), st, out);
}

fn split_address(text: &str) -> (String, u16) {
    match text.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host.to_string(), port.parse().unwrap_or(crate::session::PORT)),
        _ => (text.to_string(), crate::session::PORT),
    }
}

fn empty(ui: &mut Ui, no_usbmuxd: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
    let p = ui.painter();
    p.rect_stroke(rect, 14.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
    let pulse = (ui.input(|i| i.time) * 1.6).sin() as f32 * 0.5 + 0.5;
    p.text(rect.center_top() + vec2(0.0, 36.0), Align2::CENTER_CENTER, icon::BROADCAST, icons(26.0), mix(FAINT, MUTED, pulse));
    p.text(rect.center_top() + vec2(0.0, 72.0), Align2::CENTER_CENTER, "No Mac found yet", semi(14.0), TEXT);
    let how = if no_usbmuxd { "Start usbmuxd to see phones on a cable, or use an address below." } else { "Plug the iPhone in, open the Mac in Henge and turn on Remote Desktop." };
    p.text(rect.center_top() + vec2(0.0, 96.0), Align2::CENTER_CENTER, how, body(12.5), MUTED);
    ui.ctx().request_repaint_after_secs(0.05);
}

fn toast(ctx: &Context, st: &mut State) {
    let Some((text, since)) = st.error.clone() else { return };
    let now = ctx.input(|i| i.time);
    if now - since > 9.0 {
        st.error = None;
        return;
    }
    ctx.request_repaint_after_secs(1.0);
    Area::new(Id::new("toast")).order(Order::Tooltip).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -52.0)).show(ctx, |ui| {
        let r = Frame::new().fill(RAISED).stroke(Stroke::new(1.0, LINE)).corner_radius(12).inner_margin(vec2(14.0, 10.0)).show(ui, |ui| {
            ui.set_max_width(460.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING_CIRCLE).font(icons(17.0)).color(BAD));
                ui.label(RichText::new(&text).size(13.0));
            });
        });
        if r.response.interact(Sense::click()).clicked() {
            st.error = None;
        }
    });
}

/// Asks for the six digits Henge shows, for a Mac found on Wi-Fi that this PC has not connected to before.
fn prompt(ctx: &Context, st: &mut State, out: &mut Vec<Action>) {
    let Some(found) = st.prompt.clone() else { return };
    let screen = ctx.content_rect();
    Area::new(Id::new("dim")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(150));
        if resp.clicked() {
            st.prompt = None;
        }
    });
    Area::new(Id::new("prompt")).order(Order::Foreground).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
        Frame::new().fill(PANEL).stroke(Stroke::new(1.0, LINE)).corner_radius(16).inner_margin(22).show(ui, |ui| {
            ui.set_width(300.0);
            ui.label(RichText::new(format!("Code for {}", found.title)).font(semi(15.0)));
            ui.label(RichText::new("Henge shows six digits under Remote Desktop. They change each time sharing is turned on.").size(12.5).color(MUTED));
            ui.add_space(4.0);
            code_only(&mut st.prompt_code);
            let edit = ui.add(egui::TextEdit::singleline(&mut st.prompt_code).font(semi(22.0)).desired_width(f32::INFINITY).margin(vec2(12.0, 10.0)).horizontal_align(Align::Center));
            edit.request_focus();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = st.prompt_code.len() == 6;
                let go = ui.add_enabled_ui(ready, |ui| button(ui, "Connect", true)).inner.clicked() || (ready && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go && let Target::Net { host, port, .. } = &found.target {
                    out.push(Action::Connect(Target::Net { host: host.clone(), port: *port, code: st.prompt_code.clone() }, found.title.clone()));
                    st.prompt = None;
                }
                if button(ui, "Cancel", false).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    st.prompt = None;
                }
            });
        });
    });
}

// ---- over a running stream ----

pub struct Stream<'a> {
    pub title: &'a str,
    pub phase: Phase,
    /// The connection dropped and is being made again by itself.
    pub retrying: bool,
    pub has_picture: bool,
    pub captured: bool,
    /// The mouse goes to the Mac by position, with no capturing (see `Config::seamless`).
    pub seamless: bool,
    pub fullscreen: bool,
    pub stats: &'a Stats,
}

pub fn stream(ui: &mut Ui, st: &mut State, cfg: &mut Config, s: &Stream, out: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let full = ui.max_rect();
    let now = ctx.input(|i| i.time);
    let live = s.phase == Phase::Live && s.has_picture && !s.retrying;
    let own = st.ui_rect.take(); // as last drawn
    if !s.has_picture {
        ui.painter().rect_filled(full, 0.0, BG);
    }
    if !s.captured && !s.seamless {
        let video = ui.interact(full, Id::new("video"), Sense::click());
        if video.clicked() && live {
            out.push(Action::Capture);
        }
        if video.double_clicked() {
            out.push(Action::Fullscreen);
        }
    }

    if cfg.show_stats && s.phase == Phase::Live {
        Area::new(Id::new("stats")).order(Order::Foreground).interactable(false).anchor(Align2::LEFT_TOP, vec2(12.0, 12.0)).show(&ctx, |ui| {
            Frame::new().fill(Color32::from_black_alpha(170)).corner_radius(8).inner_margin(vec2(10.0, 7.0)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let x = s.stats;
                let line = |ui: &mut Ui, text: String| ui.label(RichText::new(text).font(FontId::monospace(11.5)).color(TEXT));
                line(ui, format!("{:.0} fps   {:.1} Mb/s", x.fps, x.mbps));
                line(ui, format!("{}×{}   {}", x.width, x.height, x.decoder));
                let mut timing = format!("decode {:.1} ms", x.decode_ms);
                if let Some(ms) = x.latency_ms {
                    timing += &format!("   behind {ms:.0} ms");
                }
                if let Some(ms) = x.rtt_ms {
                    timing += &format!("   ping {ms:.0} ms");
                }
                line(ui, timing);
            });
        });
    }

    if !live {
        let text = match (s.retrying, s.phase) {
            (true, _) => format!("Reconnecting to {}", s.title),
            (_, Phase::Connecting) => format!("Connecting to {}", s.title),
            (_, Phase::Waiting) => "Connected. Waiting for the Mac to start.".to_string(),
            _ => "Waiting for the first picture".to_string(),
        };
        Area::new(Id::new("status")).order(Order::Foreground).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(&ctx, |ui| {
            Frame::new().fill(PANEL).stroke(Stroke::new(1.0, LINE)).corner_radius(16).inner_margin(vec2(22.0, 18.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0).color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new(text).font(semi(14.0)));
                    ui.add_space(10.0);
                    if button(ui, "Cancel", false).clicked() {
                        out.push(Action::Disconnect);
                    }
                });
            });
        });
        return;
    }

    if s.captured {
        // Said once, for a few seconds, each time the Mac takes the mouse.
        let t = ctx.animate_bool_with_time(Id::new("release-hint"), now - st.captured_at < 3.5, 0.4);
        if t > 0.0 {
            pill(&ctx, "hint", t, "Left Ctrl + Left Alt gives the mouse back");
            ctx.request_repaint_after_secs(0.25);
        }
        return;
    }

    // The bar shows while the pointer is near it or has just moved, and whenever the settings are open. With
    // the seamless mouse the picture's top belongs to the Mac (its menu bar is there), so the bar opens from
    // a small grip at the edge and stays for as long as the pointer is on it.
    let hover = ctx.input(|i| i.pointer.hover_pos());
    let grip = Rect::from_center_size(pos2(full.center().x, full.top() + 6.0), vec2(84.0, 12.0));
    let near = if s.seamless {
        hover.is_some_and(|p| own.unwrap_or(grip).contains(p))
    } else {
        hover.is_some_and(|p| p.y < full.top() + 90.0) || now - st.moved < 2.5
    };
    let t = ctx.animate_bool_with_time(Id::new("bar"), near || st.settings, 0.2);
    if !s.seamless && now - st.moved < 2.6 {
        ctx.request_repaint_after_secs(0.3);
    }
    if s.seamless {
        st.ui_rect = Some(grip);
        let mark = Rect::from_center_size(pos2(full.center().x, full.top() + 4.0), vec2(40.0, 4.0));
        ui.painter().rect_filled(mark.expand(1.0), 3.0, Color32::from_black_alpha((110.0 * (1.0 - t)) as u8));
        ui.painter().rect_filled(mark, 2.0, Color32::from_white_alpha((120.0 * (1.0 - t)) as u8));
    }
    if t > 0.0 {
        let bar = Area::new(Id::new("bar")).order(Order::Foreground).anchor(Align2::CENTER_TOP, vec2(0.0, 12.0 - (1.0 - t) * 10.0)).show(&ctx, |ui| {
            ui.set_opacity(t);
            Frame::new().fill(PANEL.gamma_multiply(0.94)).stroke(Stroke::new(1.0, LINE)).corner_radius(13).inner_margin(5).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_space(10.0);
                    let (dot, _) = ui.allocate_exact_size(vec2(7.0, 32.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, GOOD);
                    ui.add_space(6.0);
                    ui.label(RichText::new(s.title).font(semi(13.0)));
                    ui.add_space(12.0);
                    let (glyph, tip) = if s.fullscreen { (icon::ARROWS_IN, "Leave full screen") } else { (icon::ARROWS_OUT, "Full screen") };
                    if icon_button(ui, glyph, tip, TEXT).clicked() {
                        out.push(Action::Fullscreen);
                    }
                    if icon_button(ui, icon::CHART_LINE, "Statistics", if cfg.show_stats { ACCENT } else { TEXT }).clicked() {
                        cfg.show_stats = !cfg.show_stats;
                    }
                    if icon_button(ui, icon::SLIDERS_HORIZONTAL, "Settings", TEXT).clicked() {
                        st.settings = !st.settings;
                    }
                    if icon_button(ui, icon::X, "Disconnect", BAD).clicked() {
                        out.push(Action::Disconnect);
                    }
                });
            });
        });
        if s.seamless {
            st.ui_rect = Some(grip.union(bar.response.rect.expand(8.0)));
        }
        pill(&ctx, "hint", t, if s.seamless { "Left Ctrl + Left Alt locks the mouse to the Mac, for games" } else { "Click the picture to control the Mac" });
    }
}

fn pill(ctx: &Context, id: &str, opacity: f32, text: &str) {
    Area::new(Id::new(id)).order(Order::Foreground).interactable(false).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -20.0)).show(ctx, |ui| {
        ui.set_opacity(opacity);
        Frame::new().fill(Color32::from_black_alpha(180)).corner_radius(15).inner_margin(vec2(14.0, 7.0)).show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.label(RichText::new(text).size(12.5).color(TEXT));
        });
    });
}

// ---- settings ----

/// `protocol2`: None with no Mac connected; otherwise whether the connected Henge takes stream settings.
pub fn settings(ctx: &Context, st: &mut State, cfg: &mut Config, protocol2: Option<bool>) {
    let t = ctx.animate_bool_with_time(Id::new("sheet"), st.settings, 0.18);
    if t == 0.0 {
        return;
    }
    let screen = ctx.content_rect();
    Area::new(Id::new("sheet-dim")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha((110.0 * t) as u8));
        if resp.clicked() {
            st.settings = false;
        }
    });
    let width = 360.0f32.min(screen.width());
    Area::new(Id::new("sheet")).order(Order::Foreground).fixed_pos(pos2(screen.right() - width * t, screen.top())).show(ctx, |ui| {
        Frame::new().fill(PANEL).stroke(Stroke::new(1.0, LINE)).inner_margin(vec2(20.0, 16.0)).show(ui, |ui| {
            ui.set_width(width - 40.0);
            ui.set_height(screen.height() - 32.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").font(semi(16.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icon::X, "Close", TEXT).clicked() {
                        st.settings = false;
                    }
                });
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(6.0);
                caps(ui, "STREAM");
                if protocol2 == Some(false) {
                    ui.label(RichText::new("The Henge on this phone is older than these settings: they apply once it is updated.").size(11.5).color(ACCENT));
                }
                let tune = protocol2 != Some(false);
                row(ui, "Quality", &format!("{} Mb/s", cfg.tuning.mbps), tune, |ui| {
                    ui.add(egui::Slider::new(&mut cfg.tuning.mbps, 2..=80).show_value(false).logarithmic(true));
                });
                row(ui, "Frame rate", "Higher is smoother and warms the phone more", tune, |ui| {
                    segmented(ui, &mut cfg.tuning.fps, &[(30, "30"), (60, "60"), (120, "120")]);
                });
                row(ui, "Picture size", "Smaller is lighter on both ends", tune, |ui| {
                    segmented(ui, &mut cfg.tuning.max_side, &[(0, "Full"), (1920, "1080p"), (1280, "720p")]);
                });
                row(ui, "Format", "HEVC looks better at the same quality setting", tune, |ui| {
                    segmented(ui, &mut cfg.tuning.hevc, &[(false, "H.264"), (true, "HEVC")]);
                });
                row(ui, "Decode on the graphics card", "Takes effect on the next connection", true, |ui| {
                    toggle(ui, &mut cfg.hardware);
                });

                ui.add_space(8.0);
                caps(ui, "SOUND");
                row(ui, "Play the Mac's sound here", "Takes effect on the next connection", true, |ui| {
                    toggle(ui, &mut cfg.sound);
                });
                row(ui, "Volume", "", cfg.sound, |ui| {
                    ui.add(egui::Slider::new(&mut cfg.volume, 0.0..=1.0).show_value(false));
                });

                ui.add_space(8.0);
                caps(ui, "MOUSE AND KEYBOARD");
                row(ui, "Seamless mouse", "The pointer goes in and out of the Mac freely", tune, |ui| {
                    toggle(ui, &mut cfg.seamless);
                });
                row(ui, "Pointer speed", &format!("{:.1}×  while the mouse is locked to the Mac", cfg.pointer_speed), true, |ui| {
                    ui.add(egui::Slider::new(&mut cfg.pointer_speed, 0.5..=4.0).show_value(false));
                });
                row(ui, "Alt is Command", "Alt and Super swap, as on a Mac keyboard", true, |ui| {
                    toggle(ui, &mut cfg.alt_is_command);
                });
                row(ui, "Controller", "Appears in the Mac as a DualShock 4 once a button is pressed", tune, |ui| {
                    toggle(ui, &mut cfg.controller);
                });
                row(ui, "Shared clipboard", "Copy here, paste in the Mac, and back", tune, |ui| {
                    toggle(ui, &mut cfg.clipboard);
                });

                ui.add_space(8.0);
                caps(ui, "APP");
                row(ui, "Connect by itself", "As soon as a phone on a cable shares a Mac", true, |ui| {
                    toggle(ui, &mut cfg.auto_connect);
                });
                row(ui, "Start at login", "Waits in the background for a Mac", true, |ui| {
                    toggle(ui, &mut cfg.start_at_login);
                });
                row(ui, "Full screen on connecting", "", true, |ui| {
                    toggle(ui, &mut cfg.fullscreen_on_connect);
                });
                row(ui, "Statistics", "Frame rate, delay and decoder over the picture", true, |ui| {
                    toggle(ui, &mut cfg.show_stats);
                });
            });
        });
    });
}

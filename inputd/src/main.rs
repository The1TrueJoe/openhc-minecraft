//! mc-inputd — USB keyboards, mice and game controllers into the game's X display.
//!
//! The game runs on a virtual X server (Xvfb) because it has no input devices of
//! its own; the picture goes to the panel through gl4es. This daemon owns the
//! real devices instead: it grabs every keyboard, mouse and pad under
//! /dev/input (hubs included — they are just more event nodes), follows
//! hotplug, and replays them into X with XTest. Keyboards and mice pass straight
//! through. A pad is mapped to Minecraft's controls, with a second mapping for
//! menus/inventories, told apart by whether the game holds the pointer grab.
//!
//! It also owns what the game's overlay draws (see ui.rs): the pointer in menus,
//! and an on-screen keyboard a pad types with.
//!
//! Environment:
//!   DISPLAY            X display to drive (default :0)
//!   MC_INPUT_IGNORE    comma-separated device-name substrings to leave alone
//!                      (default "openHC IR" — the front IR receiver)
//!   MC_PAD_LOOK        right-stick look speed, pixels per tick at full tilt (default 14)
//!   MC_PAD_CURSOR      stick cursor speed in menus (default 6)
//!   MC_PAD_DEADZONE    stick deadzone 0..1 (default 0.18)
//!   MC_GUI_SCALE       the game's GUI scale, for D-pad slot steps (default: the
//!                      game's own "auto" for the screen size)

mod evdev;
mod ui;
mod xout;

use evdev::*;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use ui::{Key, Shared, Ui};
use xout::{keycode, X};

// evdev key codes used by the pad mapping (X keycode = code + 8).
const KEY_ESC: u16 = 1;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_Q: u16 = 16;
const KEY_W: u16 = 17;
const KEY_E: u16 = 18;
const KEY_T: u16 = 20;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_SPACE: u16 = 57;
const KEY_F5: u16 = 63;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;

/// A Minecraft inventory slot is 18 GUI pixels square.
const SLOT: f32 = 18.0;
/// Held D-pad / keyboard navigation: first repeat after this, then every REPEAT.
const DELAY: Duration = Duration::from_millis(350);
const REPEAT: Duration = Duration::from_millis(110);

/// What one pad control does at a given moment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Out {
    Key(u16),
    Button(u8),
    /// One wheel notch on press (X buttons 4 up / 5 down); nothing to release.
    Wheel(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Src {
    Btn(u16),
    TrigL,
    TrigR,
    Stick(u8), // left stick as a d-pad: 0 up 1 down 2 left 3 right
    Hat(u8),   // same order
}

/// The D-pad direction a control is, if it is one (buttons or hat, per pad).
fn dpad_dir(s: Src) -> Option<usize> {
    match s {
        Src::Btn(BTN_DPAD_UP) | Src::Hat(0) => Some(0),
        Src::Btn(BTN_DPAD_DOWN) | Src::Hat(1) => Some(1),
        Src::Btn(BTN_DPAD_LEFT) | Src::Hat(2) => Some(2),
        Src::Btn(BTN_DPAD_RIGHT) | Src::Hat(3) => Some(3),
        _ => None,
    }
}

fn game_map(s: Src) -> Option<Out> {
    Some(match s {
        Src::Btn(BTN_SOUTH) => Out::Key(KEY_SPACE),      // jump
        Src::Btn(BTN_EAST) => Out::Key(KEY_LEFTSHIFT),   // sneak (hold)
        Src::Btn(BTN_X) => Out::Key(KEY_Q),              // drop item
        Src::Btn(BTN_Y) => Out::Key(KEY_E),              // inventory
        Src::Btn(BTN_TL) => Out::Wheel(4),               // hotbar left
        Src::Btn(BTN_TR) => Out::Wheel(5),               // hotbar right
        Src::Btn(BTN_THUMBL) => Out::Key(KEY_LEFTCTRL),  // sprint (hold)
        Src::Btn(BTN_THUMBR) => Out::Key(KEY_F5),        // camera
        Src::Btn(BTN_START) | Src::Btn(BTN_MODE) => Out::Key(KEY_ESC),
        Src::Btn(BTN_SELECT) => Out::Key(KEY_T),         // chat (the keyboard opens with it)
        Src::Btn(BTN_TL2) | Src::TrigL => Out::Button(3), // use / place
        Src::Btn(BTN_TR2) | Src::TrigR => Out::Button(1), // attack / mine
        Src::Stick(0) => Out::Key(KEY_W),
        Src::Stick(1) => Out::Key(KEY_S),
        Src::Stick(2) => Out::Key(KEY_A),
        Src::Stick(3) => Out::Key(KEY_D),
        Src::Btn(BTN_DPAD_LEFT) | Src::Hat(2) => Out::Wheel(4),
        Src::Btn(BTN_DPAD_RIGHT) | Src::Hat(3) => Out::Wheel(5),
        Src::Btn(BTN_DPAD_UP) | Src::Hat(0) => Out::Key(KEY_TAB), // player list (hold)
        Src::Btn(BTN_DPAD_DOWN) | Src::Hat(1) => Out::Key(KEY_Q), // drop item
        _ => return None,
    })
}

/// Menus. The D-pad is not here: it steps the pointer slot by slot (Bridge::nav).
fn menu_map(s: Src) -> Option<Out> {
    Some(match s {
        Src::Btn(BTN_SOUTH) => Out::Button(1),           // click
        Src::Btn(BTN_X) => Out::Button(3),               // right click (split a stack)
        Src::Btn(BTN_EAST) | Src::Btn(BTN_START) | Src::Btn(BTN_MODE) => Out::Key(KEY_ESC),
        Src::Btn(BTN_Y) => Out::Key(KEY_E),              // close the inventory
        Src::Btn(BTN_THUMBL) => Out::Key(KEY_LEFTSHIFT), // shift-click with A
        Src::Btn(BTN_TL) => Out::Wheel(4),               // scroll
        Src::Btn(BTN_TR) => Out::Wheel(5),
        Src::Btn(BTN_TL2) | Src::TrigL => Out::Button(3),
        Src::Btn(BTN_TR2) | Src::TrigR => Out::Button(1),
        _ => return None,
    })
}

fn env_f32(k: &str, d: f32) -> f32 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

/// The GUI scale Minecraft picks on "auto": as large as keeps the scaled screen
/// at least 320x240.
fn auto_gui_scale(w: i32, h: i32) -> i32 {
    let mut s = 1;
    while w / (s + 1) >= 320 && h / (s + 1) >= 240 {
        s += 1;
    }
    s
}

/// A pad's analog state, normalised: sticks -1..1, triggers 0..1, hat -1/0/1.
#[derive(Clone, Copy, Default)]
struct Axes {
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    lt: f32,
    rt: f32,
    hx: i32,
    hy: i32,
}

struct Pad {
    a: Axes,
    /// Which axes carry the triggers (ABS_Z/ABS_RZ on xpad, GAS/BRAKE on some).
    trig_l: u16,
    trig_r: u16,
}

enum Kind {
    Keys,
    Mouse,
    KeysMouse,
    Pad(Box<Pad>),
}

struct Dev {
    dev: Device,
    kind: Kind,
    rel: (i32, i32),
}

fn classify(d: &Device) -> Option<Kind> {
    if d.has_abs(ABS_X) && (d.has_key(BTN_SOUTH) || d.has_key(BTN_JOYSTICK)) {
        let trig_l = if d.has_abs(ABS_Z) { ABS_Z } else { ABS_BRAKE };
        let trig_r = if d.has_abs(ABS_RZ) { ABS_RZ } else { ABS_GAS };
        return Some(Kind::Pad(Box::new(Pad { a: Axes::default(), trig_l, trig_r })));
    }
    let mouse = d.has_rel(REL_X) && d.has_key(BTN_LEFT);
    let keys = d.has_key(KEY_A) || (1..BTN_MOUSE).any(|k| k != KEY_A && d.has_key(k) && k < 0x100);
    match (keys, mouse) {
        (true, true) => Some(Kind::KeysMouse),
        (true, false) => Some(Kind::Keys),
        (false, true) => Some(Kind::Mouse),
        _ => None,
    }
}

/// Map a raw absolute value to -1..1 (stick) or 0..1 (trigger) using the axis range.
fn norm(info: &AbsInfo, v: i32, centered: bool) -> f32 {
    let (lo, hi) = (info.minimum as f32, info.maximum as f32);
    if hi <= lo {
        return 0.0;
    }
    let t = (v as f32 - lo) / (hi - lo);
    if centered { t * 2.0 - 1.0 } else { t }.clamp(-1.0, 1.0)
}

/// The on-screen keyboard.
#[derive(Default)]
struct Osk {
    open: bool,
    row: i32,
    col: i32,
    shift: bool,
    /// Opened for chat, whose input line is at the bottom: keep clear of it.
    top: bool,
}

struct Bridge {
    x: Option<X>,
    display: String,
    held: HashMap<(usize, Src), Out>,
    in_game: bool,
    look: f32,
    cursor: f32,
    deadzone: f32,
    gui_scale: Option<i32>,
    frac: (f32, f32),
    osk: Osk,
    /// Directions held on a D-pad (menus) and, with the keyboard open, the left stick.
    nav: [bool; 4],
    nav_stick: [bool; 4],
    /// The direction being auto-repeated and when it fires next.
    rep: Option<(usize, Instant)>,
}

impl Bridge {
    fn emit(&mut self, o: Out, down: bool) {
        let Some(x) = &self.x else { return };
        let ok = match o {
            Out::Key(k) => x.key(keycode(k), down),
            Out::Button(b) => x.button(b, down),
            Out::Wheel(b) => !down || x.click(b),
        };
        if !ok {
            self.x = None;
        }
    }

    /// Press and release one key, with shift held around it if asked.
    fn tap(&mut self, k: u16, shift: bool) {
        if shift {
            self.emit(Out::Key(KEY_LEFTSHIFT), true);
        }
        self.emit(Out::Key(k), true);
        self.emit(Out::Key(k), false);
        if shift {
            self.emit(Out::Key(KEY_LEFTSHIFT), false);
        }
    }

    /// Drive one pad control to `on`, remembering what a press produced so the
    /// release undoes exactly that even if the mode flipped in between.
    fn set(&mut self, dev: usize, s: Src, on: bool) {
        let key = (dev, s);
        if let Some(d) = dpad_dir(s) {
            self.nav[d] = on && !self.in_game;
        }
        if !on {
            if let Some(o) = self.held.remove(&key) {
                self.emit(o, false);
            }
            return;
        }
        if self.held.contains_key(&key) {
            return;
        }
        if !self.in_game {
            if dpad_dir(s).is_some() {
                return; // stepped in tick()
            }
            if self.osk.open {
                self.osk_press(s);
                return;
            }
            if s == Src::Btn(BTN_SELECT) {
                (self.osk.open, self.osk.top) = (true, false);
                return;
            }
        }
        let m = if self.in_game { game_map(s) } else { menu_map(s) };
        if let Some(o) = m {
            self.emit(o, true);
            self.held.insert(key, o);
            if o == Out::Key(KEY_T) {
                (self.osk.open, self.osk.top) = (true, true); // chat is opening; type into it
            }
        }
    }

    /// A pad button while the on-screen keyboard is open.
    fn osk_press(&mut self, s: Src) {
        match s {
            Src::Btn(BTN_SOUTH) => match ui::key_at(self.osk.row, self.osk.col) {
                Key::Char(k) => self.tap(k, self.osk.shift),
                Key::Shift => self.osk.shift = !self.osk.shift,
                Key::Space => self.tap(KEY_SPACE, false),
                Key::Del => self.tap(KEY_BACKSPACE, false),
                Key::Enter => {
                    self.tap(KEY_ENTER, false);
                    self.osk.open = false;
                }
            },
            Src::Btn(BTN_X) => self.tap(KEY_BACKSPACE, false),
            Src::Btn(BTN_Y) => self.osk.shift = !self.osk.shift,
            Src::Btn(BTN_START) => {
                self.tap(KEY_ENTER, false);
                self.osk.open = false;
            }
            Src::Btn(BTN_EAST) | Src::Btn(BTN_SELECT) | Src::Btn(BTN_MODE) => self.osk.open = false,
            Src::Btn(BTN_TL) => self.tap(KEY_LEFT, false),
            Src::Btn(BTN_TR) => self.tap(KEY_RIGHT, false),
            _ => {}
        }
    }

    fn release_all(&mut self) {
        let held: Vec<_> = self.held.drain().collect();
        for (_, o) in held {
            self.emit(o, false);
        }
        self.nav = [false; 4];
        self.nav_stick = [false; 4];
        self.rep = None;
    }

    /// One step of D-pad / stick navigation: a key on the on-screen keyboard, or
    /// one inventory slot of pointer travel in a menu.
    fn nav_step(&mut self, d: usize) {
        if self.osk.open {
            (self.osk.row, self.osk.col) = ui::step(self.osk.row, self.osk.col, d as u8);
            return;
        }
        let Some(x) = &self.x else { return };
        let scale = self.gui_scale.unwrap_or_else(|| auto_gui_scale(x.width as i32, x.height as i32));
        let px = (SLOT * scale as f32) as i16;
        let (dx, dy) = [(0, -px), (0, px), (-px, 0), (px, 0)][d];
        if !x.motion(dx, dy) {
            self.x = None;
        }
    }

    /// Navigation with auto-repeat, run every tick.
    fn nav_tick(&mut self, now: Instant) {
        let held = (0..4).find(|&d| self.nav[d] || self.nav_stick[d]);
        match (held, self.rep) {
            (None, _) => self.rep = None,
            (Some(d), Some((r, next))) if r == d => {
                if now >= next {
                    self.nav_step(d);
                    self.rep = Some((d, next + REPEAT));
                }
            }
            (Some(d), _) => {
                self.nav_step(d);
                self.rep = Some((d, now + DELAY));
            }
        }
    }

    fn pad_tick(&mut self, idx: usize, p: &Axes) {
        let dz = self.deadzone;
        let curve = |v: f32| {
            let a = ((v.abs() - dz) / (1.0 - dz)).max(0.0);
            (a * a).copysign(v)
        };
        // Left stick: movement keys in the world (with hysteresis), cursor in menus,
        // and keyboard navigation while the on-screen keyboard is open.
        let (on, off) = (0.45, 0.30);
        let dirs = [(-p.ly, 0u8), (p.ly, 1), (-p.lx, 2), (p.lx, 3)];
        let typing = !self.in_game && self.osk.open;
        for (v, d) in dirs {
            let was = self.held.contains_key(&(idx, Src::Stick(d)));
            let now = self.in_game && if was { v > off } else { v > on };
            self.set(idx, Src::Stick(d), now);
            let was = self.nav_stick[d as usize];
            self.nav_stick[d as usize] = typing && if was { v > off } else { v > 0.6 };
        }
        // Right stick (and the left one in menus) moves the pointer.
        let (mut dx, mut dy) = (curve(p.rx), curve(p.ry));
        let speed = if self.in_game { self.look } else { self.cursor };
        if !self.in_game && !typing {
            dx += curve(p.lx);
            dy += curve(p.ly);
        }
        self.frac.0 += dx * speed;
        self.frac.1 += dy * speed;
        let (mx, my) = (self.frac.0.trunc(), self.frac.1.trunc());
        self.frac.0 -= mx;
        self.frac.1 -= my;
        if mx != 0.0 || my != 0.0 {
            if let Some(x) = &self.x {
                if !x.motion(mx as i16, my as i16) {
                    self.x = None;
                }
            }
        }
        // Analog triggers act as buttons past halfway.
        self.set(idx, Src::TrigL, p.lt > 0.5);
        self.set(idx, Src::TrigR, p.rt > 0.5);
    }

    /// What the overlay should draw now.
    fn shared(&self) -> Shared {
        let mut s = Shared::default();
        let Some(x) = &self.x else { return s };
        (s.w, s.h) = (x.width as i32, x.height as i32);
        if !self.in_game {
            if let Some((px, py)) = x.pointer() {
                (s.cursor, s.x, s.y) = (1, px as i32, py as i32);
            }
            if self.osk.open {
                (s.osk, s.row, s.col, s.shift, s.top) =
                    (1, self.osk.row, self.osk.col, self.osk.shift as i32, self.osk.top as i32);
            }
        }
        s
    }
}

fn scan(devs: &mut Vec<Dev>, skip: &mut HashSet<String>, ignore: &[String]) {
    let Ok(rd) = std::fs::read_dir("/dev/input") else { return };
    let mut paths: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.rsplit('/').next().is_some_and(|n| n.starts_with("event")))
        .collect();
    paths.sort();
    skip.retain(|p| paths.contains(p));
    for p in paths {
        if skip.contains(&p) || devs.iter().any(|d| d.dev.path == p) {
            continue;
        }
        let Ok(dev) = Device::open(&p) else { continue };
        let kind = if ignore.iter().any(|i| !i.is_empty() && dev.name.contains(i.as_str())) {
            None
        } else {
            classify(&dev)
        };
        let Some(kind) = kind else {
            skip.insert(p);
            continue;
        };
        let what = match kind {
            Kind::Pad(_) => "pad",
            Kind::Keys => "keyboard",
            Kind::Mouse => "mouse",
            Kind::KeysMouse => "keyboard+mouse",
        };
        let grabbed = dev.grab();
        eprintln!("mc-inputd: + {} \"{}\" ({}{})", p, dev.name, what, if grabbed { "" } else { ", not grabbed" });
        devs.push(Dev { dev, kind, rel: (0, 0) });
    }
}

fn main() {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
    let ignore: Vec<String> = std::env::var("MC_INPUT_IGNORE")
        .unwrap_or_else(|_| "openHC IR".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let mut b = Bridge {
        x: None,
        display,
        held: HashMap::new(),
        in_game: false,
        look: env_f32("MC_PAD_LOOK", 14.0),
        cursor: env_f32("MC_PAD_CURSOR", 6.0),
        deadzone: env_f32("MC_PAD_DEADZONE", 0.18).clamp(0.0, 0.9),
        gui_scale: std::env::var("MC_GUI_SCALE").ok().and_then(|v| v.parse().ok()).filter(|&s| s > 0),
        frac: (0.0, 0.0),
        osk: Osk::default(),
        nav: [false; 4],
        nav_stick: [false; 4],
        rep: None,
    };
    let mut ui = Ui::open();
    if ui.is_none() {
        eprintln!("mc-inputd: cannot share {} — no pointer or keyboard will be drawn", ui::PATH);
    }
    let mut devs: Vec<Dev> = Vec::new();
    let mut skip = HashSet::new();
    let (mut next_scan, mut next_x, mut next_mode) = (Instant::now(), Instant::now(), Instant::now());
    let mut next_focus = Instant::now();
    let tick = Duration::from_millis(16);
    let mut events = Vec::new();

    loop {
        let now = Instant::now();
        if now >= next_scan {
            scan(&mut devs, &mut skip, &ignore);
            next_scan = now + Duration::from_secs(1);
        }
        if b.x.is_none() && now >= next_x {
            b.x = X::connect(&b.display);
            if let Some(x) = &b.x {
                eprintln!("mc-inputd: connected to {} ({}x{})", b.display, x.width, x.height);
            }
            next_x = now + Duration::from_secs(2);
        }
        if now >= next_focus {
            if let Some(x) = &b.x {
                x.focus_game();
            }
            next_focus = now + Duration::from_secs(1);
        }
        // Menu or world decides the pad mapping and whether a pointer is drawn,
        // so it is followed whatever is plugged in.
        if now >= next_mode {
            if let Some(g) = b.x.as_ref().and_then(|x| x.pointer_grabbed()) {
                if g != b.in_game {
                    b.release_all();
                    b.in_game = g;
                    if g {
                        b.osk.open = false; // back in the world: chat or the text field closed
                    }
                }
            }
            next_mode = now + Duration::from_millis(250);
        }

        let mut fds: Vec<libc::pollfd> =
            devs.iter().map(|d| libc::pollfd { fd: d.dev.fd(), events: libc::POLLIN, revents: 0 }).collect();
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, tick.as_millis() as _) };

        let mut gone = Vec::new();
        for i in 0..devs.len() {
            events.clear();
            if devs[i].dev.read(&mut events).is_err() {
                gone.push(i);
                continue;
            }
            for ev in events.iter().copied() {
                handle(&mut b, &mut devs[i], i, ev);
            }
        }
        for i in gone.into_iter().rev() {
            let d = devs.remove(i);
            eprintln!("mc-inputd: - {} \"{}\"", d.dev.path, d.dev.name);
            // What it held must not stay pressed, and pad state is keyed by index,
            // which just shifted: a full release is the safe reset.
            b.release_all();
        }
        for i in 0..devs.len() {
            if let Kind::Pad(p) = &devs[i].kind {
                let a = p.a;
                b.pad_tick(i, &a);
            }
        }
        b.nav_tick(Instant::now());
        if let Some(u) = &mut ui {
            u.publish(b.shared());
        }
        if let Some(x) = &b.x {
            if !x.flush() {
                b.x = None;
            }
        }
    }
}

fn handle(b: &mut Bridge, d: &mut Dev, idx: usize, ev: Event) {
    match &mut d.kind {
        Kind::Pad(p) => match ev.kind {
            EV_KEY => b.set(idx, Src::Btn(ev.code), ev.value != 0),
            EV_ABS => {
                let info = d.dev.absinfo[(ev.code as usize).min(ABS_CNT - 1)];
                match ev.code {
                    ABS_X => p.a.lx = norm(&info, ev.value, true),
                    ABS_Y => p.a.ly = norm(&info, ev.value, true),
                    ABS_RX => p.a.rx = norm(&info, ev.value, true),
                    ABS_RY => p.a.ry = norm(&info, ev.value, true),
                    c if c == p.trig_l => p.a.lt = norm(&info, ev.value, false),
                    c if c == p.trig_r => p.a.rt = norm(&info, ev.value, false),
                    ABS_HAT0X | ABS_HAT0Y => {
                        if ev.code == ABS_HAT0X { p.a.hx = ev.value.signum() } else { p.a.hy = ev.value.signum() }
                        let a = p.a;
                        b.set(idx, Src::Hat(0), a.hy < 0);
                        b.set(idx, Src::Hat(1), a.hy > 0);
                        b.set(idx, Src::Hat(2), a.hx < 0);
                        b.set(idx, Src::Hat(3), a.hx > 0);
                    }
                    _ => {}
                }
            }
            _ => {}
        },
        Kind::Keys | Kind::Mouse | Kind::KeysMouse => match ev.kind {
            EV_KEY => {
                let Some(x) = &b.x else { return };
                let down = ev.value != 0;
                let ok = match ev.code {
                    BTN_LEFT => x.button(1, down),
                    BTN_RIGHT => x.button(3, down),
                    BTN_MIDDLE => x.button(2, down),
                    BTN_SIDE => x.button(8, down),
                    BTN_EXTRA => x.button(9, down),
                    c if c < 0x100 => x.key(keycode(c), down),
                    _ => true,
                };
                if !ok {
                    b.x = None;
                }
            }
            EV_REL => match ev.code {
                REL_X => d.rel.0 += ev.value,
                REL_Y => d.rel.1 += ev.value,
                REL_WHEEL => {
                    if let Some(x) = &b.x {
                        let btn = if ev.value > 0 { 4 } else { 5 };
                        for _ in 0..ev.value.unsigned_abs().min(10) {
                            x.click(btn);
                        }
                    }
                }
                _ => {}
            },
            EV_SYN => {
                let (dx, dy) = std::mem::take(&mut d.rel);
                if let Some(x) = &b.x {
                    if !x.motion(dx.clamp(-32000, 32000) as i16, dy.clamp(-32000, 32000) as i16) {
                        b.x = None;
                    }
                }
            }
            _ => {}
        },
    }
}

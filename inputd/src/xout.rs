//! The X side: inject key/button/motion events with XTest, and tell whether the
//! game currently holds a pointer grab (in-world) or not (a menu or GUI is up).

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _, EventMask, GrabMode, GrabStatus, InputFocus, MapState};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

/// X core keycodes are evdev codes + 8 under the server's evdev keymap.
pub fn keycode(evdev: u16) -> u8 {
    (evdev + 8).min(255) as u8
}

pub struct X {
    conn: RustConnection,
    root: xproto::Window,
    pub width: i16,
    pub height: i16,
}

impl X {
    pub fn connect(display: &str) -> Option<X> {
        let (conn, screen) = x11rb::connect(Some(display)).ok()?;
        let s = &conn.setup().roots[screen];
        let (root, width, height) = (s.root, s.width_in_pixels as i16, s.height_in_pixels as i16);
        // XTest must be there (Xvfb always has it); fail early rather than drop input.
        conn.xtest_get_version(2, 2).ok()?.reply().ok()?;
        Some(X { conn, root, width, height })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> bool {
        self.conn
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)
            .is_ok()
    }

    pub fn key(&self, code: u8, down: bool) -> bool {
        self.fake(if down { KEY_PRESS } else { KEY_RELEASE }, code, 0, 0)
    }

    pub fn button(&self, button: u8, down: bool) -> bool {
        self.fake(if down { BUTTON_PRESS } else { BUTTON_RELEASE }, button, 0, 0)
    }

    pub fn click(&self, button: u8) -> bool {
        self.button(button, true) && self.button(button, false)
    }

    /// Relative pointer motion (detail 1 = relative).
    pub fn motion(&self, dx: i16, dy: i16) -> bool {
        dx == 0 && dy == 0 || self.fake(MOTION_NOTIFY, 1, dx, dy)
    }

    pub fn flush(&self) -> bool {
        self.conn.flush().is_ok()
    }

    /// Where the pointer is now, in screen pixels (the game warps it, too).
    pub fn pointer(&self) -> Option<(i16, i16)> {
        let r = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
        Some((r.root_x, r.root_y))
    }

    /// True while some client (LWJGL, when the game captures the mouse) holds
    /// the pointer. We find out by trying to grab it ourselves and letting go
    /// at once; a GUI screen has released it, so ours succeeds there.
    pub fn pointer_grabbed(&self) -> Option<bool> {
        let r = self
            .conn
            .grab_pointer(
                false,
                self.root,
                EventMask::NO_EVENT,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )
            .ok()?
            .reply()
            .ok()?;
        if r.status == GrabStatus::SUCCESS {
            self.conn.ungrab_pointer(x11rb::CURRENT_TIME).ok()?;
            self.conn.flush().ok()?;
            Some(false)
        } else {
            Some(r.status == GrabStatus::ALREADY_GRABBED)
        }
    }

    /// Give the game's window the keyboard focus. There is no window manager on
    /// this display to do it, and LWJGL only takes keys and grabs the mouse
    /// while its window is focused. Picks the topmost viewable top-level
    /// window of a useful size; a no-op when it already has focus.
    pub fn focus_game(&self) -> Option<()> {
        let tree = self.conn.query_tree(self.root).ok()?.reply().ok()?;
        let mut target = None;
        for &w in tree.children.iter().rev() {
            let attr = self.conn.get_window_attributes(w).ok()?.reply().ok()?;
            if attr.map_state != MapState::VIEWABLE {
                continue;
            }
            let g = self.conn.get_geometry(w).ok()?.reply().ok()?;
            if g.width >= 200 && g.height >= 150 {
                target = Some(w);
                break;
            }
        }
        let w = target?;
        let cur = self.conn.get_input_focus().ok()?.reply().ok()?;
        if cur.focus != w {
            self.conn.set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME).ok()?;
            self.conn.flush().ok()?;
        }
        Some(())
    }
}

//! What the game's overlay draws: the pointer in menus and the on-screen keyboard.
//!
//! The game's window lives on a virtual X server and its picture reaches the panel
//! through gl4es, so no X cursor is ever seen. The overlay (runtime/overlay/
//! mc-overlay.c, loaded into the game as its libGL) draws one, and the keyboard,
//! over every frame. mc-inputd owns that state and publishes it here: a small file
//! in /tmp (shared with the game's root by mc-session's bind mount) that both
//! sides map. The layout of `Shared` and of the keyboard must match mc-overlay.c.

use std::ffi::CString;

pub const PATH: &str = "/tmp/mc-ui";
const MAGIC: u32 = 0x4955_434d; // "MCUI"

#[repr(C)]
#[derive(Default, Clone, Copy, PartialEq)]
pub struct Shared {
    magic: u32,
    seq: u32,
    /// Screen size in pixels.
    pub w: i32,
    pub h: i32,
    /// 1 while a menu is up (the game does not hold the pointer): draw the pointer.
    pub cursor: i32,
    pub x: i32,
    pub y: i32,
    /// 1 while the on-screen keyboard is open; row/col select a key, shift is sticky.
    pub osk: i32,
    pub row: i32,
    pub col: i32,
    pub shift: i32,
    /// 1 to draw the keyboard at the top of the screen (chat types at the bottom).
    pub top: i32,
}

pub struct Ui {
    p: *mut Shared,
    last: Shared,
}

impl Ui {
    pub fn open() -> Option<Ui> {
        let path = CString::new(PATH).ok()?;
        let size = std::mem::size_of::<Shared>();
        unsafe {
            let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o644);
            if fd < 0 {
                return None;
            }
            let ok = libc::ftruncate(fd, size as libc::off_t) == 0;
            let p = libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if !ok || p == libc::MAP_FAILED {
                return None;
            }
            let p = p as *mut Shared;
            std::ptr::write_volatile(p, Shared { magic: MAGIC, ..Default::default() });
            Some(Ui { p, last: Shared::default() })
        }
    }

    /// Publish `s` if it changed. A reader may see a half-written update for one
    /// frame; that only ever misplaces the pointer or a highlight briefly.
    pub fn publish(&mut self, s: Shared) {
        if s == self.last {
            return;
        }
        self.last = s;
        unsafe {
            let seq = (*self.p).seq.wrapping_add(1);
            std::ptr::write_volatile(self.p, Shared { magic: MAGIC, seq, ..s });
        }
    }
}

/// One key of the on-screen keyboard.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    /// An evdev key code, typed with shift held when the keyboard is shifted.
    Char(u16),
    Shift,
    Space,
    Del,
    Enter,
}

/// Four rows of ten character keys (US layout, as Xvfb's default keymap types
/// them), then a bottom row of four wide keys over columns 0-1, 2-5, 6-7, 8-9.
pub const ROWS: usize = 5;
pub const COLS: usize = 10;
const CHARS: [[u16; COLS]; 4] = [
    [2, 3, 4, 5, 6, 7, 8, 9, 10, 11],          // 1 2 3 4 5 6 7 8 9 0
    [16, 17, 18, 19, 20, 21, 22, 23, 24, 25],  // q w e r t y u i o p
    [30, 31, 32, 33, 34, 35, 36, 37, 38, 53],  // a s d f g h j k l /
    [44, 45, 46, 47, 48, 49, 50, 51, 52, 12],  // z x c v b n m , . -
];
const BOTTOM: [(Key, i32); 4] = [(Key::Shift, 0), (Key::Space, 2), (Key::Del, 6), (Key::Enter, 8)];

fn bottom_index(col: i32) -> usize {
    BOTTOM.iter().rposition(|&(_, c)| c <= col).unwrap_or(0)
}

pub fn key_at(row: i32, col: i32) -> Key {
    if row < 4 {
        Key::Char(CHARS[row as usize][col as usize])
    } else {
        BOTTOM[bottom_index(col)].0
    }
}

/// Move the selection one step: 0 up, 1 down, 2 left, 3 right (wrapping).
pub fn step(row: i32, col: i32, dir: u8) -> (i32, i32) {
    let rows = ROWS as i32;
    match dir {
        0 => ((row + rows - 1) % rows, snap(row - 1, col)),
        1 => ((row + 1) % rows, snap(row + 1, col)),
        _ if row == 4 => {
            let n = BOTTOM.len();
            let i = bottom_index(col);
            let i = if dir == 2 { (i + n - 1) % n } else { (i + 1) % n };
            (row, BOTTOM[i].1)
        }
        2 => (row, (col + COLS as i32 - 1) % COLS as i32),
        _ => (row, (col + 1) % COLS as i32),
    }
}

/// Entering the bottom row lands on the start column of the wide key under `col`.
fn snap(row: i32, col: i32) -> i32 {
    if row.rem_euclid(ROWS as i32) == 4 {
        BOTTOM[bottom_index(col)].1
    } else {
        col
    }
}

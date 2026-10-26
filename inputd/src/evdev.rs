//! Just enough of the Linux evdev interface: open a node, read its name and
//! capability bits, read events, and grab it so the kernel console and anything
//! else stop seeing it.

use std::ffi::CString;
use std::io;
use std::os::unix::io::RawFd;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_WHEEL: u16 = 0x08;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_GAS: u16 = 0x09;
pub const ABS_BRAKE: u16 = 0x0a;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;
pub const ABS_CNT: usize = 0x40;

pub const KEY_A: u16 = 30;
pub const BTN_MOUSE: u16 = 0x110;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const BTN_JOYSTICK: u16 = 0x120;
pub const BTN_SOUTH: u16 = 0x130;
pub const BTN_EAST: u16 = 0x131;
/// xpad reports the Xbox X button here (BTN_X == BTN_NORTH).
pub const BTN_X: u16 = 0x133;
/// xpad reports the Xbox Y button here (BTN_Y == BTN_WEST).
pub const BTN_Y: u16 = 0x134;
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_DOWN: u16 = 0x221;
pub const BTN_DPAD_LEFT: u16 = 0x222;
pub const BTN_DPAD_RIGHT: u16 = 0x223;
const KEY_CNT: usize = 0x300;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    sec: libc::c_long,
    usec: libc::c_long,
    kind: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;

pub struct Device {
    fd: RawFd,
    pub path: String,
    pub name: String,
    keys: Vec<u8>,
    rels: Vec<u8>,
    abss: Vec<u8>,
    pub absinfo: [AbsInfo; ABS_CNT],
}

fn bit(bits: &[u8], n: u16) -> bool {
    let n = n as usize;
    n / 8 < bits.len() && bits[n / 8] & (1 << (n % 8)) != 0
}

impl Device {
    pub fn open(path: &str) -> io::Result<Device> {
        let c = CString::new(path).unwrap();
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0u8; 256];
        unsafe { libc::ioctl(fd, ioc(IOC_READ, 0x06, name.len() as u64) as _, name.as_mut_ptr()) };
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let get_bits = |ev: u16, len: usize| {
            let mut v = vec![0u8; len];
            unsafe { libc::ioctl(fd, ioc(IOC_READ, 0x20 + ev as u64, len as u64) as _, v.as_mut_ptr()) };
            v
        };
        let keys = get_bits(EV_KEY, KEY_CNT / 8);
        let rels = get_bits(EV_REL, 2);
        let abss = get_bits(EV_ABS, ABS_CNT / 8);
        let mut absinfo = [AbsInfo::default(); ABS_CNT];
        for a in 0..ABS_CNT as u16 {
            if bit(&abss, a) {
                let size = std::mem::size_of::<AbsInfo>() as u64;
                unsafe { libc::ioctl(fd, ioc(IOC_READ, 0x40 + a as u64, size) as _, &mut absinfo[a as usize]) };
            }
        }
        Ok(Device {
            fd,
            path: path.to_string(),
            name: String::from_utf8_lossy(&name[..end]).into_owned(),
            keys,
            rels,
            abss,
            absinfo,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }
    pub fn has_key(&self, k: u16) -> bool {
        bit(&self.keys, k)
    }
    pub fn has_rel(&self, r: u16) -> bool {
        bit(&self.rels, r)
    }
    pub fn has_abs(&self, a: u16) -> bool {
        bit(&self.abss, a)
    }

    /// Exclusive access: the kernel VT keyboard handler (Ctrl-Alt-Del!) and any
    /// other reader stop receiving this device's events while we hold it.
    pub fn grab(&self) -> bool {
        unsafe { libc::ioctl(self.fd, ioc(IOC_WRITE, 0x90, 4) as _, 1 as libc::c_int) == 0 }
    }

    /// Drain everything readable. Err means the device went away.
    pub fn read(&self, out: &mut Vec<Event>) -> io::Result<()> {
        let mut buf = [InputEvent::default(); 64];
        loop {
            let n = unsafe {
                libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, std::mem::size_of_val(&buf))
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                return if e.kind() == io::ErrorKind::WouldBlock { Ok(()) } else { Err(e) };
            }
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let count = n as usize / std::mem::size_of::<InputEvent>();
            out.extend(buf[..count].iter().map(|e| Event { kind: e.kind, code: e.code, value: e.value }));
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

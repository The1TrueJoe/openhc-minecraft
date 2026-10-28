# openhc-minecraft

Minecraft (Java Edition 1.7.10) on a Control4 EA controller's HDMI output, on top of
[openHC](https://github.com/The1TrueJoe/openHC). Play with a USB keyboard and mouse
or a wired Xbox controller, plugged in directly or through a USB hub.

EA only for now (EA-1/EA-3: Atom CE5310, PowerVR SGX545).

## How it fits together

```
 USB keyboard / mouse / Xbox pad (hub ok)
        │  evdev (openHC usb-input feature: usbhid, xpad, uinput)
        ▼
 mc-inputd (x86_64, on the box) ──XTest──► Xvfb :0 ◄── LWJGL 2 window + input
                                                  │
 Minecraft 1.7.10 on a 32-bit Java 8 ─────────────┘
        │  OpenGL 2.1
        ▼
 mc-overlay (pointer + on-screen keyboard, from mc-inputd's /tmp/mc-ui)
        ▼
 gl4es (GL → GLES2, framebuffer mode) → PowerVR DDK 1.7 GLES2/EGL (LinuxFB WSEGL)
        ▼
 sgx545ce kernel driver (32-on-64 bridge compat) → SGX545 → /dev/fb0 → HDMI
```

The box is a 64-bit system: openHC's kernel and userspace are x86_64, and so is
mc-inputd. Only the one process that draws with the GPU is 32-bit. The only
SGX545 userspace that exists is Intel's i386 Cedarview DDK. A process can only
load libraries built for its own architecture, so the game, gl4es and the JVM
run as i386 programs under the kernel's IA32 emulation. They sit in a Debian
bookworm i386 root at `/opt/minecraft/rootfs`. openHC's on-screen WebKit view
on the EA works the same way.

## What it needs from openHC

This repo only holds what is specific to running Minecraft. Everything generic lives
upstream in openHC, so it needs an EA image with:
- the `usb-input` feature (xpad, joydev, uinput, gamepad HID drivers);
- the sgx545ce driver with the GL-client bridge compat and the device LISR. Without
  the LISR every GPU wait runs its 100 ms timeout and the game crawls at ~1 FPS;
- `S60sgx`, which brings the GPU's services up at boot, and `S03growfs`, which grows the
  root filesystem to fill its eMMC partition;
- the bus-id shim that loads libdrm itself.

## Install on a box

The box's busybox `wget` has no TLS (and its `tar` has no `-z`), so push the box side from a computer, then let
`mc-install` (which brings its own HTTPS client) do the rest:

```bash
curl -L https://github.com/The1TrueJoe/openhc-minecraft/releases/latest/download/openhc-minecraft-ea.tar.gz | gunzip | ssh root@<box> 'tar xf - -C / && /opt/minecraft/bin/mc-install'
```

`mc-install` unpacks the runtime, then downloads the game from Mojang's servers. This
repo ships no Mojang files.

Then:

```bash
ssh root@<box> /opt/minecraft/bin/mc-session start
```

The session takes the panel from the on-screen web view and hands it back when the
game quits. To start it at boot, run `touch /opt/minecraft/autostart`. To set a
player name, write it into that file.

The game runs in offline mode. Singleplayer works, and so do servers with
`online-mode=false`.

On its first run the game starts with light video settings and a render distance
of 3 chunks. The GPU's frame rate follows how much world it draws: on an EA1,
2 chunks run at about 54 FPS, 3 at about 42 and 4 at about 29. Change it in
Options → Video Settings; the game keeps what you pick.

## OptiFine (recommended)

OptiFine makes the game noticeably faster here. In the same overworld view on an
EA1, vanilla runs about 39 FPS at 3 chunks; OptiFine runs about 55 at 3 chunks and
47 at 4. Its licence doesn't allow bundling it, so the box fetches it from
optifine.net itself (or you pass it the installer jar you downloaded):

```bash
ssh root@<box> /opt/minecraft/bin/mc-optifine install
```

It checks the jar's hash, gets Mojang's launchwrapper, patches the game jar, and
seeds `optionsof.txt` with a profile tuned for this box: 4 chunks, no animated
terrain textures, fast leaves/grass/rain, no clouds. After that, settings you
change in Video Settings stick. `mc-optifine off` and `mc-optifine on` switch it
for the next start.

## Game fixes

The game starts through Mojang's launchwrapper with openHC's own tweaker
(`runtime/mod`), after OptiFine's when that is installed. It patches one thing
in the game's classes as they load:

- **Chunk lighting retries.** Every tick, 1.7.10's integrated server retries
  lighting on each chunk whose light isn't done yet, and chunks at the edge of
  the loaded area can't finish until more of the world loads around them. In a
  jungle that relighting took a whole CPU core on an EA1, and the server fell
  behind ("Can't keep up!", several seconds at a time). Retrying every 10 ticks
  instead: the server thread dropped from ~96% to ~18% of a core standing still,
  with no more "Can't keep up!", and new terrain reached the client sooner
  (43 chunks 10 s after a flight, against 17).

`MC_TWEAKS=0` in the launcher's environment leaves it out.

## Controller layout (Xbox)

| | In the world | In menus / inventory |
|---|---|---|
| Left stick | move (W A S D) | pointer |
| Right stick | look | pointer |
| D-pad | ←/→ hotbar, ↑ player list (hold), ↓ drop item | pointer, one inventory slot per press |
| RT / LT | attack-mine / use-place | left / right click |
| A | jump | click |
| B | sneak (hold) | back (Esc) |
| X | drop item | right click |
| Y | inventory | close inventory |
| LB / RB | hotbar | scroll |
| L3 | sprint (hold) | shift (shift-click with A) |
| R3 | camera (F5) | |
| Start / Guide | pause (Esc) | back |
| Back | chat, with the on-screen keyboard | on-screen keyboard |

The panel shows no X cursor (the picture never goes through X), so the overlay
draws the pointer whenever a menu is up, for a mouse as well as a pad.

**On-screen keyboard.** Back opens it — over a menu's text field, or with chat
from the world, where it sits at the top of the screen, clear of the chat line.
D-pad or left stick picks a key, A types it, X deletes, Y shifts, LB/RB move the
text cursor, Start is Enter (and closes it), B or Back closes it.

mc-inputd tells the world from a menu by whether the game holds the X pointer
grab. Tuning: `MC_PAD_LOOK`, `MC_PAD_CURSOR`, `MC_PAD_DEADZONE`, and
`MC_GUI_SCALE` if the game's GUI scale is not "auto" (the D-pad steps a slot at
that scale). Use `MC_INPUT_IGNORE` to leave devices alone; the front IR receiver
is ignored by default.

## Layout

| Path | What |
|---|---|
| `inputd/` | `mc-inputd` (evdev → XTest) and `mc-fetch` (HTTPS for the box), Rust, static musl |
| `box/` | what goes on the box: `mc-session`, `mc-install`, `S96minecraft` |
| `runtime/` | builds `runtime-i386.tar.xz`: Debian i386 + Xvfb + gl4es (+ patches) + the overlay + the game fixes (`mod/`) + Zulu 8 i686 + launchers |
| `.github/workflows/build.yml` | builds both artifacts; attaches them to a release |

## Build

CI does it (`build.yml`). By hand, on an x86_64 Linux host:

```bash
sudo runtime/build-runtime.sh out
```

```bash
cd inputd && CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --target x86_64-unknown-linux-musl
```

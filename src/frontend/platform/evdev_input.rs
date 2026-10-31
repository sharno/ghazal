use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use evdev::{Device, EventType, Key as KeyCode};

use crate::device::DeviceConfig;
use crate::frontend::input::{Button, Key};

const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_HAT0X: u16 = 16;
const ABS_HAT0Y: u16 = 17;

struct Pad {
    device: Device,
    path: PathBuf,
    /// The handheld's own controls. Losing them restarts the menu; a Bluetooth or USB
    /// pad that disconnects is just dropped.
    builtin: bool,
    /// (min, max) of ABS_X / ABS_Y if the device has an analog stick.
    stick: [Option<(i32, i32)>; 2],
    /// Current digital direction per axis (-1, 0, 1) for hat0 x/y and stick x/y.
    axis_state: [i8; 4],
    gate: Gate,
}

enum Gate { WaitingForNeutral, Ready }

/// How an input device takes part in the menu.
#[derive(Debug, PartialEq)]
enum Role {
    /// The handheld's gamepad.
    Builtin,
    /// A gamepad that comes and goes: Bluetooth or USB.
    Extra,
    /// Volume, power and the like: only tells a held Menu it is part of a chord.
    Companion,
}

fn role(builtin_name: Option<&str>, name: &str, gamepad: bool) -> Role {
    match builtin_name {
        Some(filter) if name.contains(filter) => Role::Builtin,
        Some(_) if gamepad => Role::Extra,
        None if gamepad => Role::Builtin,
        Some(_) | None => Role::Companion,
    }
}

/// New device nodes in /dev/input: a Bluetooth or USB pad being connected.
struct Hotplug(OwnedFd);

impl Hotplug {
    fn open() -> Option<Self> {
        // SAFETY: a plain syscall; the descriptor is wrapped in an OwnedFd at once.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is a fresh, valid descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: the path is a NUL-terminated literal and fd is an inotify descriptor.
        let watch = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c"/dev/input".as_ptr(), libc::IN_CREATE) };
        (watch >= 0).then_some(Self(fd))
    }

    /// Discards the pending notifications; true if there were any.
    fn drain(&self) -> bool {
        let mut buffer = [0u8; 1024];
        let mut any = false;
        // SAFETY: reads into a buffer we own, up to its length, from a descriptor we own.
        while unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {
            any = true;
        }
        any
    }
}

pub struct Input {
    pads: Vec<Pad>,
    /// Other key devices (volume, power). Never grabbed: their keys only tell
    /// a held Menu/Start that it is part of a daemon hotkey chord.
    companions: Vec<Device>,
    map: Vec<(u16, Button)>,
    /// Exclusive grab while in the foreground (device.toml input.grab).
    grab: bool,
    grabbed: bool,
    /// device.toml's name filter for the handheld's own pad; other gamepads are extra.
    builtin_name: Option<String>,
    hotplug: Option<Hotplug>,
}

impl Input {
    /// Opens the gamepad named in device.toml, waiting a few seconds for it to appear
    /// (at boot, input-bridge/muinput may still be creating it).
    pub fn open(dev: &DeviceConfig) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Self::try_open(dev) {
                Ok(input) => return Ok(input),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    fn try_open(dev: &DeviceConfig) -> Result<Self> {
        let builtin_name = dev.input.gamepad.clone();
        let mut pads = Vec::new();
        let mut companions = Vec::new();
        for (path, device) in evdev::enumerate() {
            let name = device.name().unwrap_or("").to_string();
            match role(builtin_name.as_deref(), &name, is_gamepad(&device)) {
                Role::Companion => {
                    if device.supported_events().contains(EventType::KEY) && set_nonblocking(&device).is_ok() {
                        companions.push(device);
                    }
                }
                Role::Builtin => pads.push(Pad::open(path, device, &name, true)?),
                // A stray pad must never keep the menu from starting.
                Role::Extra => match Pad::open(path, device, &name, false) {
                    Ok(pad) => pads.push(pad),
                    Err(error) => eprintln!("ghazal: input {name}: {error:#}"),
                },
            }
        }
        if !pads.iter().any(|pad| pad.builtin) {
            bail!("no gamepad {:?} in /dev/input (see `ghazal probe-input`)", dev.input.gamepad);
        }

        let names = [
            ("a", Button::A), ("b", Button::B), ("x", Button::X), ("y", Button::Y),
            ("l1", Button::L1), ("r1", Button::R1), ("l2", Button::L2), ("r2", Button::R2),
            ("select", Button::Select), ("start", Button::Start), ("menu", Button::Menu),
            // Only for pads whose d-pad sends keys; hats and sticks are always handled.
            ("up", Button::Up), ("down", Button::Down), ("left", Button::Left), ("right", Button::Right),
        ];
        let map = names.iter().filter_map(|(n, b)| dev.button(n).map(|code| (code, *b))).collect();
        let mut input = Self { pads, companions, map, grab: dev.input.grab, grabbed: false, builtin_name, hotplug: Hotplug::open() };
        input.acquire()?;
        Ok(input)
    }

    /// Fresh descriptors have no gameplay backlog. Held controls must return to
    /// neutral before this frontend session accepts any input.
    fn acquire(&mut self) -> Result<()> {
        let grab = self.grab;
        let mut builtin_failure = None;
        self.pads.retain_mut(|pad| match pad.acquire(grab) {
            Ok(()) => true,
            Err(error) if pad.builtin => {
                builtin_failure = Some(error);
                true
            }
            Err(error) => {
                eprintln!("ghazal: input {}: {error:#}", pad.path.display());
                false
            }
        });
        if let Some(error) = builtin_failure {
            return Err(error);
        }
        self.grabbed = grab;
        Ok(())
    }

    /// Picks up gamepads connected since the last look: a Bluetooth controller
    /// that just paired, or a USB one.
    fn adopt_new_pads(&mut self) {
        for (path, device) in evdev::enumerate() {
            let name = device.name().unwrap_or("").to_string();
            if !is_gamepad(&device) || self.pads.iter().any(|pad| pad.path == path) || role(self.builtin_name.as_deref(), &name, true) != Role::Extra {
                continue;
            }
            let adopted = Pad::open(path, device, &name, false).and_then(|mut pad| pad.acquire(self.grab).map(|()| pad));
            match adopted {
                Ok(pad) => self.pads.push(pad),
                Err(error) => eprintln!("ghazal: new input {name}: {error:#}"),
            }
        }
    }

    pub fn release(&mut self) {
        if self.grabbed {
            for pad in &mut self.pads {
                let _ = pad.device.ungrab();
            }
            self.grabbed = false;
        }
    }

    /// Waits up to `timeout` for input and reports (key, pressed) edges.
    pub fn poll(&mut self, timeout: Duration, wake_fd: Option<std::os::fd::RawFd>, mut emit: impl FnMut(Key, bool)) -> Result<()> {
        let mut fds: Vec<libc::pollfd> = self
            .pads
            .iter()
            .map(|p| p.device.as_raw_fd())
            .chain(self.companions.iter().map(Device::as_raw_fd))
            .chain(self.hotplug.iter().map(|hotplug| hotplug.0.as_raw_fd()))
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        if let Some(fd) = wake_fd { fds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 }); }
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: fds is a valid, correctly sized pollfd array.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(err.into());
        }
        let hung = |fd: &libc::pollfd| fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0;
        if wake_fd.is_some() && fds.last().is_some_and(hung) {
            bail!("levels notification descriptor failed");
        }
        // The supervisor restarts the menu, which reopens the gamepad.
        if self.pads.iter().zip(&fds).any(|(pad, fd)| pad.builtin && hung(fd)) { bail!("gamepad disconnected"); }
        // A removed keyboard or Bluetooth pad reports POLLHUP without POLLIN forever:
        // drop it, or poll() would never sleep again.
        let mut pad_gone: Vec<bool> = fds[..self.pads.len()].iter().map(hung).collect();
        let gone: Vec<bool> = fds[self.pads.len()..][..self.companions.len()].iter().map(hung).collect();
        let hotplugged = self.hotplug.as_ref().is_some_and(|hotplug| fds[self.pads.len() + self.companions.len()].revents & libc::POLLIN != 0 && hotplug.drain());

        for (device, fd) in self.companions.iter_mut().zip(&fds[self.pads.len()..]) {
            if fd.revents & libc::POLLIN == 0 {
                continue;
            }
            let events: Vec<_> = match device.fetch_events() {
                Ok(events) => events.collect(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e.into()),
            };
            for ev in events.iter().filter(|ev| ev.event_type() == EventType::KEY && ev.value() != 2) {
                emit(Key::Other(ev.code()), ev.value() == 1);
            }
        }
        let mut index = 0;
        self.companions.retain(|_| { index += 1; !gone[index - 1] });
        for (index, (pad, fd)) in self.pads.iter_mut().zip(&fds).enumerate() {
            if fd.revents & libc::POLLIN == 0 || pad_gone[index] {
                continue;
            }
            let events: Vec<_> = match pad.device.fetch_events() {
                Ok(events) => events.collect(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                // The extra pad disappeared between poll and read.
                Err(_) if !pad.builtin => { pad_gone[index] = true; continue; }
                Err(e) => return Err(e.into()),
            };
            if matches!(pad.gate, Gate::WaitingForNeutral) {
                if pad.neutral()? { pad.gate = Gate::Ready; }
                continue;
            }
            for ev in events {
                match ev.event_type() {
                    // value 2 is kernel autorepeat; we do our own.
                    EventType::KEY if ev.value() != 2 => {
                        let key = self.map.iter().find(|(code, _)| *code == ev.code()).map_or(Key::Other(ev.code()), |&(_, b)| Key::Button(b));
                        emit(key, ev.value() == 1);
                    }
                    EventType::ABSOLUTE => {
                        let (slot, dir) = match ev.code() {
                            ABS_HAT0X => (0, ev.value().signum() as i8),
                            ABS_HAT0Y => (1, ev.value().signum() as i8),
                            ABS_X | ABS_Y => {
                                let i = (ev.code() - ABS_X) as usize;
                                let Some(range) = pad.stick[i] else { continue };
                                (2 + i, stick_direction(ev.value(), range, pad.axis_state[2 + i]))
                            }
                            _ => continue,
                        };
                        let prev = std::mem::replace(&mut pad.axis_state[slot], dir);
                        if prev != dir {
                            let horizontal = slot % 2 == 0;
                            let button = |d: i8| match (horizontal, d > 0) {
                                (true, false) => Button::Left,
                                (true, true) => Button::Right,
                                (false, false) => Button::Up,
                                (false, true) => Button::Down,
                            };
                            if prev != 0 {
                                emit(Key::Button(button(prev)), false);
                            }
                            if dir != 0 {
                                emit(Key::Button(button(dir)), true);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut index = 0;
        self.pads.retain(|_| { index += 1; !pad_gone[index - 1] });
        if hotplugged {
            self.adopt_new_pads();
        }
        Ok(())
    }
}

impl Pad {
    fn open(path: PathBuf, device: Device, name: &str, builtin: bool) -> Result<Self> {
        set_nonblocking(&device)?;
        let abs = device.get_abs_state().ok();
        let has_abs = |code: u16| device.supported_absolute_axes().is_some_and(|a| a.contains(evdev::AbsoluteAxisType(code)));
        let range = |code: u16| {
            let info = abs.as_ref()?[code as usize];
            (has_abs(code) && info.maximum > info.minimum).then_some((info.minimum, info.maximum))
        };
        let stick = [range(ABS_X), range(ABS_Y)];
        eprintln!("ghazal: using input {} ({name}) stick={stick:?}", path.display());
        Ok(Self { device, path, builtin, stick, axis_state: [0; 4], gate: Gate::WaitingForNeutral })
    }

    /// Starts listening: held controls must first return to neutral, and the
    /// device is grabbed if device.toml asks for it.
    fn acquire(&mut self, grab: bool) -> Result<()> {
        self.gate = if self.neutral()? { Gate::Ready } else { Gate::WaitingForNeutral };
        if grab {
            if let Err(e) = self.device.grab() {
                eprintln!("ghazal: grab failed: {e}");
            }
        }
        Ok(())
    }

    fn neutral(&self) -> Result<bool> {
        if self.device.get_key_state()?.iter().next().is_some() { return Ok(false); }
        if self.device.supported_absolute_axes().is_some() {
            let axes = self.device.get_abs_state()?;
            for code in [ABS_HAT0X, ABS_HAT0Y] {
                if self.device.supported_absolute_axes().is_some_and(|supported| supported.contains(evdev::AbsoluteAxisType(code)))
                    && axes[code as usize].value != 0 { return Ok(false); }
            }
            for (axis, range) in self.stick.iter().enumerate() {
                if let Some(range) = range {
                    if stick_direction(axes[axis].value, *range, 1) != 0 { return Ok(false); }
                }
            }
        }
        Ok(true)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.release();
    }
}

/// Maps an analog axis to -1/0/1 with hysteresis so the cursor doesn't flicker at the edge.
fn stick_direction(value: i32, (min, max): (i32, i32), current: i8) -> i8 {
    let center = (min + max) / 2;
    let half = ((max - min) / 2).max(1);
    let norm = (value - center) as f32 / half as f32;
    let threshold = if current == 0 { 0.6 } else { 0.4 };
    if norm <= -threshold {
        -1
    } else if norm >= threshold {
        1
    } else {
        0
    }
}

fn is_gamepad(device: &Device) -> bool {
    device
        .supported_keys()
        .is_some_and(|k| k.contains(KeyCode::BTN_SOUTH) || k.contains(KeyCode::BTN_EAST) || k.contains(KeyCode::BTN_DPAD_UP))
}

fn set_nonblocking(device: &Device) -> Result<()> {
    let fd = device.as_raw_fd();
    // SAFETY: plain fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            bail!("fcntl O_NONBLOCK: {}", std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// `--probe-input`: print every input device and then every event, so you can fill in
/// the `[input]` section of the config for a new device.
pub fn probe() -> Result<()> {
    let mut devices: Vec<_> = evdev::enumerate().collect();
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, device) in &devices {
        println!(
            "{}  {:?}  gamepad={}  keys={}",
            path.display(),
            device.name().unwrap_or("?"),
            is_gamepad(device),
            device.supported_keys().map_or(0, |k| k.iter().count()),
        );
    }
    println!("\nPress buttons (Ctrl+C to stop)…");
    for (_, device) in &devices {
        set_nonblocking(device)?;
    }
    let started = Instant::now();
    loop {
        let mut fds: Vec<libc::pollfd> = devices
            .iter()
            .map(|(_, d)| libc::pollfd { fd: d.as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
        // SAFETY: valid pollfd array.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        for ((path, device), fd) in devices.iter_mut().zip(&fds) {
            if fd.revents & libc::POLLIN == 0 {
                continue;
            }
            let Ok(events) = device.fetch_events() else { continue };
            for ev in events {
                let t = started.elapsed().as_secs_f32();
                match ev.event_type() {
                    EventType::KEY => println!(
                        "{t:8.3} {}  KEY  code={:<4} {:?} value={}",
                        path.display(), ev.code(), KeyCode::new(ev.code()), ev.value()
                    ),
                    EventType::ABSOLUTE => println!(
                        "{t:8.3} {}  ABS  code={:<4} {:?} value={}",
                        path.display(), ev.code(), evdev::AbsoluteAxisType(ev.code()), ev.value()
                    ),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_gamepads_never_stand_in_for_the_handhelds_own() {
        assert_eq!(role(Some("H700 Gamepad"), "H700 Gamepad", true), Role::Builtin);
        assert_eq!(role(Some("H700 Gamepad"), "Wireless Controller", true), Role::Extra);
        assert_eq!(role(Some("H700 Gamepad"), "gpio-keys", false), Role::Companion);
        // Without a name filter every gamepad is the handheld's.
        assert_eq!(role(None, "Any Pad", true), Role::Builtin);
        assert_eq!(role(None, "Keys", false), Role::Companion);
    }
}

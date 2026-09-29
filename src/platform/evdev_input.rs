use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use evdev::{Device, EventType, Key};

use crate::config::InputConfig;
use crate::input::Button;

const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_HAT0X: u16 = 16;
const ABS_HAT0Y: u16 = 17;

struct Pad {
    device: Device,
    /// (min, max) of ABS_X / ABS_Y if the device has an analog stick.
    stick: [Option<(i32, i32)>; 2],
    /// Current digital direction per axis (-1, 0, 1) for hat0 x/y and stick x/y.
    axis_state: [i8; 4],
}

pub struct Input {
    pads: Vec<Pad>,
    map: Vec<(u16, Button)>,
    grabbed: bool,
}

impl Input {
    pub fn open(config: &InputConfig) -> Result<Self> {
        let mut pads = Vec::new();
        for (path, device) in evdev::enumerate() {
            let name = device.name().unwrap_or("").to_string();
            let wanted = match &config.device_name {
                Some(filter) => name.contains(filter.as_str()),
                None => is_gamepad(&device),
            };
            if !wanted {
                continue;
            }
            set_nonblocking(&device)?;
            let abs = device.get_abs_state().ok();
            let has_abs = |code: u16| device.supported_absolute_axes().is_some_and(|a| a.contains(evdev::AbsoluteAxisType(code)));
            let range = |code: u16| {
                let info = abs.as_ref()?[code as usize];
                (has_abs(code) && info.maximum > info.minimum).then_some((info.minimum, info.maximum))
            };
            let stick = [range(ABS_X), range(ABS_Y)];
            eprintln!("oxmux: using input {} ({name}) stick={stick:?}", path.display());
            pads.push(Pad { device, stick, axis_state: [0; 4] });
        }
        if pads.is_empty() {
            bail!("no gamepad found in /dev/input (try --probe-input, then set input.device_name)");
        }

        let c = config;
        let map = vec![
            (c.a, Button::A), (c.b, Button::B), (c.x, Button::X), (c.y, Button::Y),
            (c.l1, Button::L1), (c.r1, Button::R1), (c.l2, Button::L2), (c.r2, Button::R2),
            (c.select, Button::Select), (c.start, Button::Start), (c.menu, Button::Menu),
            (c.up, Button::Up), (c.down, Button::Down), (c.left, Button::Left), (c.right, Button::Right),
        ];
        let mut input = Self { pads, map, grabbed: false };
        input.acquire();
        Ok(input)
    }

    /// Exclusive grab so muOS daemons don't also react to our navigation.
    pub fn acquire(&mut self) {
        for pad in &mut self.pads {
            // Drop anything queued while the emulator ran.
            while let Ok(events) = pad.device.fetch_events() {
                if events.count() == 0 {
                    break;
                }
            }
            pad.axis_state = [0; 4];
            if let Err(e) = pad.device.grab() {
                eprintln!("oxmux: grab failed: {e}");
            }
        }
        self.grabbed = true;
    }

    pub fn release(&mut self) {
        if self.grabbed {
            for pad in &mut self.pads {
                let _ = pad.device.ungrab();
            }
            self.grabbed = false;
        }
    }

    /// Waits up to `timeout` for input and reports (button, pressed) edges.
    pub fn poll(&mut self, timeout: Duration, mut emit: impl FnMut(Button, bool)) -> Result<()> {
        let mut fds: Vec<libc::pollfd> = self
            .pads
            .iter()
            .map(|p| libc::pollfd { fd: p.device.as_raw_fd(), events: libc::POLLIN, revents: 0 })
            .collect();
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

        for (pad, fd) in self.pads.iter_mut().zip(&fds) {
            if fd.revents & libc::POLLIN == 0 {
                continue;
            }
            let events: Vec<_> = match pad.device.fetch_events() {
                Ok(events) => events.collect(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e.into()),
            };
            for ev in events {
                match ev.event_type() {
                    // value 2 is kernel autorepeat; we do our own.
                    EventType::KEY if ev.value() != 2 => {
                        if let Some(&(_, b)) = self.map.iter().find(|(code, _)| *code == ev.code()) {
                            emit(b, ev.value() == 1);
                        }
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
                                emit(button(prev), false);
                            }
                            if dir != 0 {
                                emit(button(dir), true);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
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
        .is_some_and(|k| k.contains(Key::BTN_SOUTH) || k.contains(Key::BTN_EAST) || k.contains(Key::BTN_DPAD_UP))
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
                        path.display(), ev.code(), Key::new(ev.code()), ev.value()
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

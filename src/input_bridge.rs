//! `oxmux input-bridge`: native replacement for muOS's `muinput`.
//!
//! Grabs the raw gpio pad and republishes it through uinput with muinput's identity and
//! code mapping (device.toml `[input_bridge]`), so RetroArch autoconfig and the frontend
//! see the same "muOS-Keys" device. Force-feedback rumble requests are played on the
//! vibration motor from `[rumble]`.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{
    AbsInfo, AbsoluteAxisType, AttributeSet, BusType, Device, EventType, FFEffectType, InputEvent, InputId, Key,
    UinputAbsSetup,
};

use crate::device::{DeviceConfig, InputBridge};
use crate::sys;

const MAX_EFFECTS: u32 = 16;

pub fn run(dev: &DeviceConfig) -> Result<()> {
    let cfg = dev.input_bridge.as_ref().ok_or_else(|| anyhow!("device.toml has no [input_bridge]"))?;
    let keys = parse_map(&cfg.keys)?;
    let axes = parse_map(&cfg.axes)?;

    let mut source = find_source(&cfg.source, Duration::from_secs(10))?;
    source.grab().context("grabbing source device")?;
    let mut out = build_output(cfg, &source, &keys, &axes)?;
    eprintln!("oxmux-bridge: {:?} -> {:?}", cfg.source, cfg.name);
    set_nonblocking(source.as_raw_fd())?;
    set_nonblocking(out.as_raw_fd())?;

    // effect id -> replay length
    let mut effects: HashMap<i16, Duration> = HashMap::new();
    let mut motor_off_at: Option<Instant> = None;
    let mut batch: Vec<InputEvent> = Vec::new();

    loop {
        let mut fds = [
            libc::pollfd { fd: source.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: out.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let timeout = motor_off_at.map_or(-1, |t| t.saturating_duration_since(Instant::now()).as_millis() as i32);
        // SAFETY: valid pollfd array.
        unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };

        if motor_off_at.is_some_and(|t| Instant::now() >= t) {
            motor(dev, false);
            motor_off_at = None;
        }

        if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            bail!("source device went away");
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if let Ok(events) = source.fetch_events() {
                for ev in events {
                    match ev.event_type() {
                        EventType::SYNCHRONIZATION => {
                            if !batch.is_empty() {
                                out.emit(&batch)?;
                                batch.clear();
                            }
                        }
                        EventType::KEY => {
                            let code = keys.get(&ev.code()).copied().unwrap_or(ev.code());
                            batch.push(InputEvent::new(EventType::KEY, code, ev.value()));
                        }
                        EventType::ABSOLUTE => {
                            let code = axes.get(&ev.code()).copied().unwrap_or(ev.code());
                            batch.push(InputEvent::new(EventType::ABSOLUTE, code, ev.value()));
                        }
                        _ => {}
                    }
                }
            }
        }

        if fds[1].revents & libc::POLLIN != 0 {
            let requests: Vec<_> = match out.fetch_events() {
                Ok(evs) => evs.collect(),
                Err(_) => Vec::new(),
            };
            for ev in requests {
                const UI_FF_UPLOAD: u16 = 1;
                const UI_FF_ERASE: u16 = 2;
                match (ev.event_type(), ev.code()) {
                    (EventType::UINPUT, UI_FF_UPLOAD) => {
                        let mut upload = out.process_ff_upload(ev)?;
                        // Updating an existing effect keeps its id; new ones get the first free slot.
                        let id = match upload.effect_id() {
                            id if id >= 0 && effects.contains_key(&id) => Some(id),
                            _ => (0..MAX_EFFECTS as i16).find(|i| !effects.contains_key(i)),
                        };
                        match id {
                            Some(id) => {
                                upload.set_effect_id(id);
                                let length = Duration::from_millis(upload.effect().replay.length.max(1) as u64);
                                effects.insert(id, length);
                                upload.set_retval(0);
                            }
                            None => upload.set_retval(-libc::ENOSPC),
                        }
                    }
                    (EventType::UINPUT, UI_FF_ERASE) => {
                        let erase = out.process_ff_erase(ev)?;
                        effects.remove(&(erase.effect_id() as i16));
                    }
                    // Play (value > 0) or stop (0) an uploaded effect.
                    (EventType::FORCEFEEDBACK, id) => {
                        if ev.value() > 0 {
                            let length = effects.get(&(id as i16)).copied().unwrap_or(Duration::from_millis(200));
                            motor(dev, true);
                            motor_off_at = Some(Instant::now() + length.min(Duration::from_secs(5)));
                        } else {
                            motor(dev, false);
                            motor_off_at = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn motor(dev: &DeviceConfig, on: bool) {
    if let Some(r) = &dev.rumble {
        let _ = sys::write_file(&r.path, if on { &r.on } else { "0" });
    }
}

fn parse_map(map: &std::collections::BTreeMap<String, u16>) -> Result<HashMap<u16, u16>> {
    map.iter()
        .map(|(k, v)| Ok((k.parse::<u16>().with_context(|| format!("bad code {k:?} in input_bridge"))?, *v)))
        .collect()
}

fn find_source(name: &str, timeout: Duration) -> Result<Device> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((_, d)) = evdev::enumerate().find(|(_, d)| d.name() == Some(name)) {
            return Ok(d);
        }
        if Instant::now() >= deadline {
            bail!("input device {name:?} not found");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn build_output(cfg: &InputBridge, source: &Device, keys: &HashMap<u16, u16>, axes: &HashMap<u16, u16>) -> Result<VirtualDevice> {
    let mut out_keys = AttributeSet::<Key>::new();
    if let Some(src) = source.supported_keys() {
        for k in src.iter() {
            out_keys.insert(Key::new(keys.get(&k.code()).copied().unwrap_or(k.code())));
        }
    }
    let mut builder = VirtualDeviceBuilder::new()?
        .name(cfg.name.as_str())
        .input_id(InputId::new(BusType(cfg.bus), cfg.vendor, cfg.product, cfg.version))
        .with_keys(&out_keys)?;

    let abs_state = source.get_abs_state().ok();
    if let (Some(src_axes), Some(state)) = (source.supported_absolute_axes(), abs_state) {
        for a in src_axes.iter() {
            let info = state[a.0 as usize];
            let code = axes.get(&a.0).copied().unwrap_or(a.0);
            let abs = AbsInfo::new(info.value, info.minimum, info.maximum, info.fuzz, info.flat, info.resolution);
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType(code), abs))?;
        }
    }

    let mut ff = AttributeSet::<FFEffectType>::new();
    ff.insert(FFEffectType::FF_RUMBLE);
    builder = builder.with_ff(&ff)?.with_ff_effects_max(MAX_EFFECTS);
    Ok(builder.build()?)
}

fn set_nonblocking(fd: i32) -> Result<()> {
    // SAFETY: fcntl on an fd we own.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) } < 0 {
        bail!("fcntl: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

pub mod evdev_input;
mod fbdev;
mod kms;

#[cfg(feature = "desktop")]
pub mod desktop;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::ComponentHandle;
use slint::{PhysicalSize, Rgb8Pixel};

use crate::device::{DeviceConfig, Output};
use crate::frontend::app::{App, Effect};
use crate::frontend::config::FrontendConfig;
use crate::frontend::input::Repeater;
use crate::ui::AppWindow;

/// Frame interval while Slint animations are running.
const ANIMATION_FRAME: Duration = Duration::from_millis(16);

/// Slint platform with no windowing system: one software-rendered window that we
/// blit to the framebuffer (or the simulator window) ourselves.
struct OxPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
    /// Added to real time. Snapshots use it to fast-forward animations.
    skew: Rc<Cell<Duration>>,
}

impl Platform for OxPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed() + self.skew.get()
    }
}

/// The rendered frame. Slint keeps this buffer between frames and only repaints
/// regions that changed.
pub struct Screen {
    window: Rc<MinimalSoftwareWindow>,
    pixels: Vec<Rgb8Pixel>,
    width: usize,
    height: usize,
    skew: Rc<Cell<Duration>>,
    full_redraw: bool,
}

/// Dirty rectangle `(x, y, w, h)` of the last render.
pub type Damage = (usize, usize, usize, usize);

impl Screen {
    /// Installs the Slint platform. Must run before any Slint component is created.
    pub fn install(width: u32, height: u32) -> Result<Self> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(PhysicalSize::new(width, height));
        let skew = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(OxPlatform {
            window: window.clone(),
            start: Instant::now(),
            skew: skew.clone(),
        }))
        .map_err(|e| anyhow!("installing Slint platform: {e}"))?;
        let (width, height) = (width as usize, height as usize);
        Ok(Self { window, pixels: vec![Rgb8Pixel::default(); width * height], width, height, skew, full_redraw: true })
    }

    pub fn pixels(&self) -> &[Rgb8Pixel] {
        &self.pixels
    }

    /// Advances animations and timers, then repaints if anything changed.
    pub fn render(&mut self) -> Option<Damage> {
        slint::platform::update_timers_and_animations();
        let mut damage = None;
        let full = std::mem::take(&mut self.full_redraw);
        self.window.draw_if_needed(|renderer| {
            let region = renderer.render(&mut self.pixels, self.width);
            let (o, s) = (region.bounding_box_origin(), region.bounding_box_size());
            if s.width > 0 && s.height > 0 {
                damage = Some((o.x as usize, o.y as usize, s.width as usize, s.height as usize));
            }
        });
        if full {
            damage = Some((0, 0, self.width, self.height));
        }
        damage
    }

    /// Makes the next render report the whole screen as damaged, e.g. after an emulator
    /// drew over the framebuffer. Our pixel buffer itself is still intact.
    pub fn invalidate(&mut self) {
        self.full_redraw = true;
    }

    /// How long the event loop may sleep before Slint needs another frame.
    pub fn next_wake(&self) -> Option<Duration> {
        if self.window.has_active_animations() {
            return Some(ANIMATION_FRAME);
        }
        slint::platform::duration_until_next_timer_update()
    }

    /// Jumps the animation clock forward (snapshot mode).
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn fast_forward(&self, by: Duration) {
        self.skew.set(self.skew.get() + by);
    }
}

pub fn run(config: FrontendConfig, device: &DeviceConfig) -> Result<()> {
    #[cfg(feature = "desktop")]
    return desktop::run(config, device);
    #[cfg(not(feature = "desktop"))]
    return run_device(config, device);
}

fn build_app(config: FrontendConfig) -> Result<App> {
    let ui = AppWindow::new().map_err(|e| anyhow!("creating UI: {e}"))?;
    let mut logo = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(64, 64);
    logo.make_mut_bytes().copy_from_slice(include_bytes!(concat!(env!("OUT_DIR"), "/ghazal-logo.rgba")));
    ui.set_brand_logo(slint::Image::from_rgba8_premultiplied(logo));
    ui.global::<crate::ui::Theme>().set_compact(ui.window().size().width < 400);
    ui.show().map_err(|e| anyhow!("showing UI: {e}"))?;
    App::new(config, ui)
}

/// `/run` is cleared on boot. A supervisor restart must not relaunch a broken
/// emulator repeatedly, or apply a newly changed startup setting mid-session.
#[cfg_attr(feature = "desktop", allow(dead_code))]
fn claim_boot_startup() -> Result<bool> {
    let directory = std::path::Path::new("/run/ghazal");
    std::fs::create_dir_all(directory)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join("startup-claimed")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}
/// Where frames go on the device: fbdev (vendor kernels) or DRM/KMS (mainline).
enum Display {
    Fbdev(fbdev::Framebuffer),
    Kms(kms::Kms),
}

impl Display {
    fn open(device: &DeviceConfig) -> Result<Self> {
        Ok(match device.display.output {
            Output::Fbdev => Self::Fbdev(fbdev::Framebuffer::open(&device.framebuffer)?),
            Output::Kms => Self::Kms(kms::Kms::open(&device.display.card)?.with_rotation(device.display.rotation)),
        })
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Fbdev(fb) => fb.size(),
            Self::Kms(k) => k.size(),
        }
    }

    fn present(&mut self, frame: &[Rgb8Pixel], stride: usize, damage: Damage) -> Result<()> {
        match self {
            Self::Fbdev(fb) => fb.present(frame, stride, damage),
            Self::Kms(k) => k.present(frame, stride, damage)?,
        }
        Ok(())
    }

    /// The display stays exclusively borrowed until the emulator exits and its
    /// original scanout has been restored.
    fn with_released<T>(&mut self, run: impl FnOnce() -> T) -> Result<T> {
        match self {
            Self::Fbdev(fb) => {
                let result = run();
                fb.reload()?;
                Ok(result)
            }
            Self::Kms(k) => k.with_released(run),
        }
    }
}

/// On-device loop: fbdev or KMS output, evdev input, sleeping in poll() between events.
#[cfg_attr(feature = "desktop", allow(dead_code))]
fn run_device(config: FrontendConfig, device: &DeviceConfig) -> Result<()> {
    let mut display = Display::open(device)?;
    let mut input = evdev_input::Input::open(device)?;
    let (w, h) = display.size();
    let mut screen = Screen::install(w, h)?;
    let levels_watch = crate::levels::Watch::open(&config.daemon_socket)?;
    let mut app = build_app(config)?;
    if !claim_boot_startup()? { app.disable_startup(); }
    let mut repeater = Repeater::default();
    let mut first_frame = true;

    loop {
        let now = Instant::now();
        app.tick(now);
        if let Some(damage) = screen.render() {
            display.present(screen.pixels(), w as usize, damage)?;
            if std::mem::take(&mut first_frame) {
                // On screen: tell `ghazal init` this boot worked.
                crate::init::mark_boot_ok();
            }
        }

        let mut effect = app.take_startup_effect();
        if effect.is_none() {
            let mut deadline = repeater.next_deadline().map_or(app.next_deadline(), |t| t.min(app.next_deadline()));
            if let Some(wake) = screen.next_wake() {
                deadline = deadline.min(now + wake);
            }
            input.poll(deadline.saturating_duration_since(now), levels_watch.as_ref().map(crate::levels::Watch::fd), |key, pressed| {
                if pressed {
                    repeater.press(key, Instant::now());
                } else {
                    repeater.release(key);
                }
            })?;
            if levels_watch.as_ref().map(crate::levels::Watch::drain).transpose()?.unwrap_or(false) {
                app.refresh_levels();
            }
            repeater.tick(Instant::now());

            let buttons: Vec<_> = repeater.drain().collect();
            effect = buttons.into_iter().find_map(|button| app.update(button));
        }
        match effect {
            None => {}
            Some(Effect::Quit) => return Ok(()),
            Some(Effect::Launch { spec, game }) => {
                input.release();
                let result = display.with_released(|| spec.run(|| app.launch_started(game)))?;
                screen.invalidate();
                input.acquire();
                repeater.clear();
                app.launch_finished(result);
            }
        }
    }
}

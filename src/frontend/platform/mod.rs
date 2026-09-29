pub mod evdev_input;
mod fbdev;

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

use crate::device::DeviceConfig;
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
    ui.show().map_err(|e| anyhow!("showing UI: {e}"))?;
    Ok(App::new(config, ui))
}

/// On-device loop: fbdev output, evdev input, sleeping in poll() between events.
#[cfg_attr(feature = "desktop", allow(dead_code))]
fn run_device(config: FrontendConfig, device: &DeviceConfig) -> Result<()> {
    let mut fb = fbdev::Framebuffer::open(&device.framebuffer)?;
    let mut input = evdev_input::Input::open(device)?;
    let (w, h) = fb.size();
    let mut screen = Screen::install(w, h)?;
    let mut app = build_app(config)?;
    let mut repeater = Repeater::default();
    let mut first_frame = true;

    loop {
        let now = Instant::now();
        app.tick(now);
        if let Some(damage) = screen.render() {
            fb.present(screen.pixels(), w as usize, damage);
            if std::mem::take(&mut first_frame) {
                // On screen: tell `oxmux init` this boot worked.
                crate::init::mark_boot_ok();
            }
        }

        let mut deadline = repeater.next_deadline().map_or(app.next_deadline(), |t| t.min(app.next_deadline()));
        if let Some(wake) = screen.next_wake() {
            deadline = deadline.min(now + wake);
        }
        input.poll(deadline.saturating_duration_since(now), |button, pressed| {
            if pressed {
                repeater.press(button, Instant::now());
            } else {
                repeater.release(button);
            }
        })?;
        repeater.tick(Instant::now());

        let buttons: Vec<_> = repeater.drain().collect();
        for button in buttons {
            match app.update(button) {
                None => {}
                Some(Effect::Quit) => return Ok(()),
                Some(Effect::Launch(spec)) => {
                    input.release();
                    let result = spec.run();
                    fb.reload()?;
                    screen.invalidate();
                    input.acquire();
                    repeater.clear();
                    app.launch_finished(result);
                    break;
                }
            }
        }
    }
}

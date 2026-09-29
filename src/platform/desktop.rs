//! Desktop simulator: shows the same 640x480 Slint frame in a window, keyboard as gamepad.

use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use super::{build_app, Screen};
use crate::app::{App, Effect};
use crate::config::Config;
use crate::input::{Button, Repeater};

/// RG40XXV panel resolution.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

struct Sim {
    app: App,
    screen: Screen,
    repeater: Repeater,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    error: Option<anyhow::Error>,
}

pub fn run(config: Config) -> Result<()> {
    let screen = Screen::install(WIDTH, HEIGHT)?;
    let app = build_app(config)?;
    let event_loop = EventLoop::new()?;
    let mut sim = Sim { app, screen, repeater: Repeater::default(), window: None, surface: None, error: None };
    event_loop.run_app(&mut sim)?;
    sim.error.map_or(Ok(()), Err)
}

/// Headless: render the first frame, then one frame after each button in `buttons`
/// (comma-separated, e.g. "down,a,menu"), as `DIR/NN.png`. Animations are fast-forwarded
/// to their end state and launches are only printed.
pub fn snapshot(config: Config, dir: &Path, buttons: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut screen = Screen::install(WIDTH, HEIGHT)?;
    let mut app = build_app(config)?;
    let mut frame = 0;
    let mut save = |screen: &mut Screen| -> Result<()> {
        screen.fast_forward(Duration::from_secs(1));
        screen.render();
        let path = dir.join(format!("{frame:02}.png"));
        let mut encoder = png::Encoder::new(std::fs::File::create(&path)?, WIDTH, HEIGHT);
        encoder.set_color(png::ColorType::Rgb);
        let bytes: Vec<u8> = screen.pixels().iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        encoder.write_header()?.write_image_data(&bytes)?;
        frame += 1;
        Ok(())
    };
    save(&mut screen)?;
    for name in buttons.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some(button) = parse_button(name) else { bail!("unknown button {name:?}") };
        if let Some(Effect::Launch(spec)) = app.update(button) {
            eprintln!("oxmux: would launch {} {:?}", spec.program.display(), spec.args);
        }
        save(&mut screen)?;
    }
    Ok(())
}

fn parse_button(name: &str) -> Option<Button> {
    Some(match name.to_ascii_lowercase().as_str() {
        "up" => Button::Up,
        "down" => Button::Down,
        "left" => Button::Left,
        "right" => Button::Right,
        "a" => Button::A,
        "b" => Button::B,
        "x" => Button::X,
        "y" => Button::Y,
        "l1" => Button::L1,
        "r1" => Button::R1,
        "select" => Button::Select,
        "start" => Button::Start,
        "menu" => Button::Menu,
        _ => return None,
    })
}

fn map_key(key: KeyCode) -> Option<Button> {
    Some(match key {
        KeyCode::ArrowUp => Button::Up,
        KeyCode::ArrowDown => Button::Down,
        KeyCode::ArrowLeft => Button::Left,
        KeyCode::ArrowRight => Button::Right,
        KeyCode::KeyZ | KeyCode::Enter => Button::A,
        KeyCode::KeyX | KeyCode::Backspace => Button::B,
        KeyCode::KeyS => Button::X,
        KeyCode::KeyA => Button::Y,
        KeyCode::KeyQ => Button::L1,
        KeyCode::KeyW => Button::R1,
        KeyCode::Digit1 => Button::L2,
        KeyCode::Digit2 => Button::R2,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Button::Select,
        KeyCode::Space => Button::Start,
        KeyCode::Escape | KeyCode::Tab => Button::Menu,
        _ => return None,
    })
}

impl Sim {
    fn present(&mut self) -> Result<()> {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return Ok(()) };
        let size = window.inner_size();
        let (Some(ww), Some(wh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        surface.resize(ww, wh).map_err(|e| anyhow!("{e}"))?;

        // Nearest-neighbour scale so pixels look like they will on the handheld.
        let src = self.screen.pixels();
        let (sw, sh) = (WIDTH as usize, HEIGHT as usize);
        let (dw, dh) = (size.width as usize, size.height as usize);
        let mut buf = surface.buffer_mut().map_err(|e| anyhow!("{e}"))?;
        for y in 0..dh {
            let row = &src[(y * sh / dh) * sw..][..sw];
            for x in 0..dw {
                let p = row[x * sw / dw];
                buf[y * dw + x] = (p.r as u32) << 16 | (p.g as u32) << 8 | p.b as u32;
            }
        }
        buf.present().map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, e: anyhow::Error) {
        self.error = Some(e);
        event_loop.exit();
    }
}

impl ApplicationHandler for Sim {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("oxmux (RG40XXV sim)")
            .with_inner_size(PhysicalSize::new(WIDTH * 2, HEIGHT * 2));
        let result = (|| -> Result<()> {
            let window = Rc::new(event_loop.create_window(attrs)?);
            let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow!("{e}"))?;
            let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow!("{e}"))?;
            self.window = Some(window);
            self.surface = Some(surface);
            Ok(())
        })();
        if let Err(e) = result {
            self.fail(event_loop, e);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.present() {
                    self.fail(event_loop, e);
                }
            }
            WindowEvent::Resized(_) => self.screen.invalidate(),
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let Some(button) = map_key(code) else { return };
                match event.state {
                    ElementState::Pressed => self.repeater.press(button, Instant::now()),
                    ElementState::Released => self.repeater.release(button),
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.repeater.tick(now);
        self.app.tick(now);

        let buttons: Vec<_> = self.repeater.drain().collect();
        for button in buttons {
            match self.app.update(button) {
                None => {}
                Some(Effect::Quit) => return event_loop.exit(),
                Some(Effect::Launch(spec)) => {
                    // Blocks the window while the emulator runs, same as on the device.
                    let result = spec.run();
                    self.repeater.clear();
                    self.app.launch_finished(result);
                    break;
                }
            }
        }

        if self.screen.render().is_some() {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        let mut deadline = self.repeater.next_deadline().map_or(self.app.next_deadline(), |t| t.min(self.app.next_deadline()));
        if let Some(wake) = self.screen.next_wake() {
            deadline = deadline.min(now + wake);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

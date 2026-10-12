//! Box art for the selected game: found on the card, or downloaded from
//! libretro's thumbnail server into a cache, and decoded off the UI thread.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::config::{ArtSources, FrontendConfig};
use super::library::{Game, System};

/// Thumbnails are scaled to fit this box (the art panel) when loaded or cached.
pub const MAX_WIDTH: u32 = 240;
pub const MAX_HEIGHT: u32 = 300;
const SERVER: &str = "thumbnails.libretro.com";

/// Art files that may exist for a game, best first.
pub fn candidates(config: &FrontendConfig, system: &System, game: &Game) -> Vec<PathBuf> {
    let Some(stem) = game.path.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = game.path.parent().map(|dir| dir.join("Imgs").join(format!("{stem}.png"))).into_iter().collect();
    if let (Some(catalogue), Some(muos)) = (&config.muos_catalogue, &system.art.muos) {
        paths.push(catalogue.join(muos).join("box").join(format!("{stem}.png")));
    }
    paths.extend(cached(config, &system.art, stem));
    paths
}

fn cached(config: &FrontendConfig, art: &ArtSources, stem: &str) -> Option<PathBuf> {
    Some(config.thumbnail_cache.as_ref()?.join(art.libretro.as_ref()?).join(format!("{}.png", libretro_name(stem))))
}

/// libretro replaces these characters in thumbnail file names.
fn libretro_name(stem: &str) -> String {
    stem.chars().map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c }).collect()
}

/// Decodes a PNG and scales it down (box filter) to fit MAX_WIDTH x MAX_HEIGHT.
pub fn load(path: &Path) -> Result<SharedPixelBuffer<Rgba8Pixel>> {
    let mut decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut data = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut data)?;
    let rgba: Vec<[u8; 4]> = match frame.color_type {
        png::ColorType::Rgba => data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect(),
        png::ColorType::Rgb => data.chunks_exact(3).map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => bail!("unexpanded palette in {}", path.display()),
    };
    Ok(scale_to_fit(&rgba[..(frame.width * frame.height) as usize], frame.width, frame.height))
}

fn scale_to_fit(src: &[[u8; 4]], width: u32, height: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let scale = (MAX_WIDTH as f32 / width as f32).min(MAX_HEIGHT as f32 / height as f32).min(1.0);
    let (w, h) = (((width as f32 * scale) as u32).max(1), ((height as f32 * scale) as u32).max(1));
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let pixels = out.make_mut_slice();
    for y in 0..h {
        let (y0, y1) = (y * height / h, ((y + 1) * height / h).max(y * height / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * width / w, ((x + 1) * width / w).max(x * width / w + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = src[(sy * width + sx) as usize];
                    for (s, c) in sum.iter_mut().zip(p) { *s += c as u32; }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            pixels[(y * w + x) as usize] = Rgba8Pixel { r: (sum[0] / n) as u8, g: (sum[1] / n) as u8, b: (sum[2] / n) as u8, a: (sum[3] / n) as u8 };
        }
    }
    out
}

/// Decodes requested art on a worker so browsing never waits for PNG decoding.
pub struct Loader {
    requests: Sender<(u64, Vec<PathBuf>)>,
    results: Receiver<(u64, Option<SharedPixelBuffer<Rgba8Pixel>>)>,
    next: u64,
}

impl Loader {
    pub fn new() -> Result<Self> {
        let (requests, jobs) = channel::<(u64, Vec<PathBuf>)>();
        let (done, results) = channel();
        std::thread::Builder::new().name("oxmux-art".into()).spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                // Only the latest request matters while the cursor is moving.
                while let Ok(newer) = jobs.try_recv() { job = newer; }
                let (id, paths) = job;
                let image = paths.iter().filter(|p| p.is_file()).find_map(|p| load(p).inspect_err(|e| eprintln!("oxmux: art {}: {e:#}", p.display())).ok());
                if done.send((id, image)).is_err() { break; }
            }
        })?;
        Ok(Self { requests, results, next: 0 })
    }

    /// Starts loading; the result carries the returned id.
    pub fn request(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.next += 1;
        let _ = self.requests.send((self.next, paths));
        self.next
    }

    pub fn try_recv(&self) -> Option<(u64, Option<SharedPixelBuffer<Rgba8Pixel>>)> {
        self.results.try_recv().ok()
    }
}

/// One game whose art should be fetched into the cache.
pub struct Job {
    url_path: String,
    destination: PathBuf,
}

/// Games that have no art yet and whose system names libretro's art.
pub fn missing(config: &FrontendConfig, systems: &[System]) -> Vec<Job> {
    let mut jobs = Vec::new();
    for system in systems {
        let Some(libretro) = &system.art.libretro else { continue };
        for game in &system.games {
            let Some(stem) = game.path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Some(destination) = cached(config, &system.art, stem) else { continue };
            if candidates(config, system, game).iter().any(|p| p.is_file()) || destination.with_extension("missing").exists() {
                continue;
            }
            jobs.push(Job { url_path: format!("/{}/Named_Boxarts/{}.png", percent_encode(libretro), percent_encode(&libretro_name(stem))), destination });
        }
    }
    jobs
}

pub enum Progress {
    Fetched { done: usize, total: usize },
    Finished { found: usize, total: usize },
    Failed(String),
}

/// Finds games without art and fetches theirs in a background thread,
/// reporting progress. Scanning there keeps thousands of file checks off the UI.
pub fn download(config: FrontendConfig) -> Result<Receiver<Progress>> {
    let (progress, receiver) = channel();
    std::thread::Builder::new().name("oxmux-art-download".into()).spawn(move || {
        let library = super::library::Library::scan(&config);
        let jobs = missing(&config, library.systems());
        let total = jobs.len();
        let (mut found, mut failures) = (0, 0);
        for (done, job) in jobs.into_iter().enumerate() {
            match fetch(&job) {
                Ok(true) => { found += 1; failures = 0; }
                Ok(false) => failures = 0,
                Err(error) => {
                    failures += 1;
                    if failures >= 5 {
                        let _ = progress.send(Progress::Failed(format!("{error:#}")));
                        return;
                    }
                }
            }
            if progress.send(Progress::Fetched { done: done + 1, total }).is_err() { return; }
        }
        let _ = progress.send(Progress::Finished { found, total });
    })?;
    Ok(receiver)
}

/// Downloads one thumbnail, scaled for the panel. A missing one is marked so
/// it isn't requested again.
fn fetch(job: &Job) -> Result<bool> {
    std::fs::create_dir_all(job.destination.parent().context("cache path has no parent")?)?;
    let Some(png_bytes) = http_get(SERVER, &job.url_path)? else {
        File::create(job.destination.with_extension("missing"))?;
        return Ok(false);
    };
    let downloaded = job.destination.with_extension("download");
    std::fs::write(&downloaded, &png_bytes)?;
    let scaled = load(&downloaded);
    std::fs::remove_file(&downloaded)?;
    let scaled = scaled?;
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, scaled.width(), scaled.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = scaled.as_slice().iter().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
        encoder.write_header()?.write_image_data(&bytes)?;
    }
    crate::sys::write_atomic(&job.destination, &encoded)?;
    Ok(true)
}

/// Plain HTTP/1.1 GET. `None` for 404; thumbnails aren't secret, and this
/// avoids a TLS stack in the static binary.
fn http_get(host: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let address = (host, 80).to_socket_addrs()?.next().with_context(|| format!("resolving {host}"))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: oxmux\r\nConnection: close\r\n\r\n")?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").context("malformed HTTP response")?;
    let head = std::str::from_utf8(&response[..split])?;
    let status: u16 = head.split_whitespace().nth(1).context("missing HTTP status")?.parse()?;
    let body = &response[split + 4..];
    match status {
        200 if head.lines().any(|l| l.eq_ignore_ascii_case("transfer-encoding: chunked")) => Ok(Some(dechunk(body)?)),
        200 => Ok(Some(body.to_vec())),
        404 => Ok(None),
        other => bail!("{host}{path}: HTTP {other}"),
    }
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n").context("malformed chunk")?;
        let size = usize::from_str_radix(std::str::from_utf8(&body[..end])?.split(';').next().unwrap_or("").trim(), 16)?;
        if size == 0 { return Ok(out); }
        let chunk = body.get(end + 2..end + 2 + size).context("truncated chunk")?;
        out.extend_from_slice(chunk);
        body = &body[end + 4 + size..];
    }
}

fn percent_encode(text: &str) -> String {
    text.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_urls_follow_libretro() {
        assert_eq!(libretro_name("Ys I & II: Ancient (USA)"), "Ys I _ II_ Ancient (USA)");
        assert_eq!(percent_encode("Super Metroid (Japan, USA)"), "Super%20Metroid%20%28Japan%2C%20USA%29");
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(), b"abcde");
    }

    #[test]
    fn scaling_fits_the_panel_and_keeps_small_images() {
        let big = scale_to_fit(&vec![[10, 20, 30, 255]; 480 * 600], 480, 600);
        assert_eq!((big.width(), big.height()), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(big.as_slice()[0], Rgba8Pixel { r: 10, g: 20, b: 30, a: 255 });
        let small = scale_to_fit(&[[1, 2, 3, 4]; 4], 2, 2);
        assert_eq!((small.width(), small.height()), (2, 2));
    }
}

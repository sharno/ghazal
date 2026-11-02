/// Width and height of each status icon's pixels (src/frontend/indicators.rs).
const ICON_SIZE: u32 = 40;

fn main() {
    // Rasterize the vector mark at build time; the handheld needs no SVG decoder.
    println!("cargo:rerun-if-changed=assets/ghazal.svg");
    let svg = std::fs::read("assets/ghazal.svg").expect("reading gazelle logo");
    let tree = resvg::usvg::Tree::from_data(&svg, &Default::default()).expect("parsing gazelle logo");
    let mut pixels = resvg::tiny_skia::Pixmap::new(64, 64).expect("allocating logo");
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(0.25, 0.25), &mut pixels.as_mut());
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("ghazal-logo.rgba"), pixels.data()).expect("embedding gazelle logo");
    // Status bar icons, white on transparent for Slint to colorize, at twice
    // the size they are drawn so scaling down keeps their edges smooth.
    for name in ["wifi-dot", "wifi-near", "wifi-mid", "wifi-far", "bluetooth", "syncthing", "hdmi"] {
        let source = format!("assets/icons/{name}.svg");
        println!("cargo:rerun-if-changed={source}");
        let tree = resvg::usvg::Tree::from_data(&std::fs::read(&source).expect(&source), &Default::default()).expect(&source);
        let mut pixels = resvg::tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE).expect("allocating an icon");
        let scale = ICON_SIZE as f32 / tree.size().width();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixels.as_mut());
        std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join(format!("{name}.rgba")), pixels.data()).expect(&source);
    }
    // Fonts are embedded as TTF and rasterized at runtime, so any character in a game
    // title renders (pre-rendered glyphs would only cover characters seen in .slint files).
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config).expect("compiling ui/app.slint");

    // fontconfig's dlopen loader links `-ldl`. musl has dlopen in libc itself and Rust's
    // self-contained musl doesn't ship a libdl.a, so provide an empty one.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        std::fs::write(out.join("libdl.a"), b"!<arch>\n").expect("writing empty libdl.a");
        println!("cargo:rustc-link-search=native={}", out.display());
    }
}

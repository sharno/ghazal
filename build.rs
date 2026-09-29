fn main() {
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

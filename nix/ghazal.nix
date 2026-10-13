# Static aarch64 (musl) ghazal, the same binary `cargo device` builds: prebuilt Rust
# toolchain + musl std from rust-overlay, dependencies vendored from Cargo.lock, linked
# with the bundled rust-lld (see .cargo/config.toml). No C cross toolchain involved.
{ lib, stdenv, rust-bin, rustPlatform, src }:

let
  toolchain = rust-bin.stable.latest.minimal.override {
    targets = [ "aarch64-unknown-linux-musl" ];
  };
  target = "aarch64-unknown-linux-musl";
in
stdenv.mkDerivation {
  pname = "ghazal";
  version = (lib.importTOML (src + "/Cargo.toml")).package.version;

  src = lib.cleanSourceWith {
    inherit src;
    # Only what the build reads, so docs/scripts edits don't trigger rebuilds.
    filter = path: _type:
      let rel = lib.removePrefix (toString src + "/") (toString path); in
      builtins.any (p: rel == p || lib.hasPrefix "${p}/" rel)
        [ "Cargo.toml" "Cargo.lock" "build.rs" ".cargo" "src" "ui" "assets" "config" ];
  };

  # Copy only this file into the store: image tooling/docs must not invalidate the
  # vendor derivation and force a full device rebuild.
  cargoDeps = rustPlatform.importCargoLock { lockFile = ../Cargo.lock; };
  nativeBuildInputs = [ toolchain rustPlatform.cargoSetupHook ];

  buildPhase = ''
    runHook preBuild
    cargo build --release --offline --target ${target}
    runHook postBuild
  '';

  installPhase = ''
    install -Dm755 target/${target}/release/ghazal $out/bin/ghazal
  '';

  # Cross-compiled; nothing to run on the build machine.
  doCheck = false;
  dontFixup = true;

  meta = {
    description = "Rust userland for Anbernic H700 handhelds";
    license = lib.licenses.gpl3Plus;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
  };
}

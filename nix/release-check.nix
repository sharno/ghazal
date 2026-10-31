# The release gates (tools/release-check): runtime layout and the headless
# RetroArch input test. Its unit tests run during the build.
{ lib, rustPlatform, src }:
rustPlatform.buildRustPackage {
  pname = "ghazal-release-check";
  version = "0.1.0";
  src = lib.cleanSource src;
  cargoRoot = "tools/release-check";
  buildAndTestSubdir = "tools/release-check";
  cargoLock.lockFile = ../tools/release-check/Cargo.lock;
  meta.description = "Release gates for the Ghazal device runtime";
}

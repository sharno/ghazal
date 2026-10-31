# SDL2_image and SDL2_mixer for the SDL2 build in sdl2.nix, with only what
# PortMaster's GUI and dialogs use: PNG and JPEG through stb, WAV, OGG and MP3
# through the decoders bundled in SDL2_mixer. nixpkgs' versions link
# sdl2-compat over SDL3 and pull in a codec library per format.
{ lib, stdenv, cmake, pkg-config, SDL2_image, SDL2_mixer, sdl2 }:
let
  # sdl2.pc joins these onto the prefix, so they must be relative.
  paths = [ (lib.cmakeFeature "CMAKE_INSTALL_LIBDIR" "lib") (lib.cmakeFeature "CMAKE_INSTALL_INCLUDEDIR" "include") ];
  options = prefix: enabled: disabled:
    map (name: lib.cmakeBool "${prefix}${name}" true) enabled ++ map (name: lib.cmakeBool "${prefix}${name}" false) disabled;
  build = { pname, upstream, prefix, enabled, disabled, description, extra ? [ ] }: stdenv.mkDerivation {
    inherit pname;
    inherit (upstream) version src;
    strictDeps = true;
    nativeBuildInputs = [ cmake pkg-config ];
    buildInputs = [ sdl2 ];
    cmakeFlags = paths ++ options prefix enabled disabled ++ extra;
    meta = upstream.meta // { inherit description; };
  };
in
{
  SDL2_image = build {
    pname = "SDL2_image-h700";
    upstream = SDL2_image;
    prefix = "SDL2IMAGE_";
    enabled = [ "BACKEND_STB" "PNG" "JPG" ];
    disabled = [ "VENDORED" "DEPS_SHARED" "SAMPLES" "AVIF" "JXL" "TIF" "WEBP" ];
    description = "SDL2_image with PNG and JPEG";
  };
  SDL2_mixer = build {
    pname = "SDL2_mixer-h700";
    upstream = SDL2_mixer;
    prefix = "SDL2MIXER_";
    enabled = [ "WAVE" "MP3" ];
    disabled = [ "VENDORED" "DEPS_SHARED" "SAMPLES" "CMD" "FLAC" "GME" "MOD" "MIDI" "OPUS" "WAVPACK" ];
    extra = [ (lib.cmakeFeature "SDL2MIXER_VORBIS" "STB") ];
    description = "SDL2_mixer with WAV, OGG and MP3";
  };
}

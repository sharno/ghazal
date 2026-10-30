# Classic SDL2 (as in ROCKNIX) for standalone PPSSPP: KMSDRM video with EGL
# and GLES, ALSA audio and evdev/udev input. nixpkgs' SDL2 is sdl2-compat over
# SDL3 with X11, Wayland, PipeWire and PulseAudio.
{ lib, stdenv, fetchurl, cmake, pkg-config, mesa, libdrm, alsa-lib, udev, rocknix }:
stdenv.mkDerivation (finalAttrs: {
  pname = "sdl2-h700";
  version = "2.32.10";
  src = fetchurl {
    url = "https://github.com/libsdl-org/SDL/releases/download/release-${finalAttrs.version}/SDL2-${finalAttrs.version}.tar.gz";
    hash = "sha256-X1mTxTDwhFNcZaaHnpsmrUQRabPiXXidgyhwQKnKUWU=";
  };
  # Current alsa-lib declares snd_pcm_info_free as returning void.
  patches = [ "${rocknix}/projects/ROCKNIX/packages/graphics/SDL2/patches/0009-fix-snd-pcm-build.patch" ];
  strictDeps = true;
  nativeBuildInputs = [ cmake pkg-config ];
  buildInputs = [ mesa libdrm alsa-lib udev ];
  # sdl2.pc joins these onto the prefix, so they must be relative.
  cmakeFlags = [ (lib.cmakeFeature "CMAKE_INSTALL_LIBDIR" "lib") (lib.cmakeFeature "CMAKE_INSTALL_INCLUDEDIR" "include") ]
    ++ map (name: lib.cmakeBool "SDL_${name}" true) [ "KMSDRM" "OPENGLES" "ALSA" "LIBUDEV" ]
    ++ map (name: lib.cmakeBool "SDL_${name}" false) [
      # Link what is used rather than dlopen it, so the Nix closure is complete.
      "KMSDRM_SHARED" "ALSA_SHARED"
      "X11" "WAYLAND" "VULKAN" "OPENGL" "DIRECTFB" "OFFSCREEN" "PULSEAUDIO" "PIPEWIRE"
      "JACK" "SNDIO" "OSS" "NAS" "ESD" "ARTS" "LIBSAMPLERATE" "DBUS" "IBUS"
      "STATIC" "TEST" "RPATH"
    ];
  # SDL always dlopens libEGL/libGLESv2 and libudev by soname.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath [ mesa udev ]} $out/lib/libSDL2-2.0.so.0.*
  '';
  meta = {
    description = "SDL2 for KMSDRM handhelds";
    homepage = "https://libsdl.org";
    license = lib.licenses.zlib;
    platforms = lib.platforms.linux;
  };
})

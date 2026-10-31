# PPSSPP with GLES on the H700, from nixpkgs' release source: the standalone
# SDL build, or with `libretroCore = true` the core used by left-rotated panels.
# Flags follow ROCKNIX's H700 build; FFmpeg is PPSSPP's bundled static build.
{ lib, stdenv, ppsspp, cmake, pkg-config, python3, mesa, libzip, snappy, zlib,
  sdl2, SDL2_ttf, libretroCore ? false }:
stdenv.mkDerivation {
  pname = if libretroCore then "ppsspp-libretro-h700" else "ppsspp-h700";
  inherit (ppsspp) version src;
  strictDeps = true;
  enableParallelBuilding = true;
  # Built like ROCKNIX's, without nixpkgs' hardening flags (see rocknix-core.nix).
  hardeningDisable = [ "all" ];
  nativeBuildInputs = [ cmake pkg-config python3 ];
  buildInputs = [ mesa libzip snappy zlib ] ++ lib.optionals (!libretroCore) [ sdl2 SDL2_ttf ];
  # SDL_ttf.h includes "SDL.h", but PPSSPP's text renderer gets no SDL include path.
  env = lib.optionalAttrs (!libretroCore) { NIX_CFLAGS_COMPILE = "-I${sdl2}/include/SDL2"; };
  # Settings stay on the OS card (/var); game saves are shared with the
  # libretro core on the data card, states and screenshots go to SAVES.
  postPatch = ''
    substituteInPlace git-version.cmake --replace-fail unknown v${ppsspp.version}
  '' + lib.optionalString (!libretroCore) ''
    substituteInPlace UI/NativeApp.cpp \
      --replace-fail /usr/share $out/share \
      --replace-fail 'g_Config.memStickDirectory = Path(config) / "ppsspp";' \
        'g_Config.memStickDirectory = Path("/mnt/games/SAVES/file/PPSSPP");'
    substituteInPlace Core/Util/PathUtil.cpp \
      --replace-fail 'pspDirectory = memStickDirectory / "PSP";' \
        'pspDirectory = Path("/var/lib/ghazal/ppsspp/PSP");' \
      --replace-fail 'return pspDirectory / "SAVEDATA";' 'return memStickDirectory / "PSP/SAVEDATA";' \
      --replace-fail 'return pspDirectory / "SCREENSHOT";' 'return Path("/mnt/games/SAVES/screenshots");' \
      --replace-fail 'return pspDirectory / "PPSSPP_STATE";' 'return Path("/mnt/games/SAVES/state/PPSSPP-standalone");'
    # States are named after the ROM file, as RetroArch's and ROCKNIX's
    # builds name them, so states saved by ROCKNIX's PPSSPP still load.
    substituteInPlace Core/SaveState.cpp --replace-fail 'return GenerateFullDiscId(paramSFO);' \
      'return PSP_CoreParameter().fileToStart.WithReplacedExtension("").GetFilename();'
  '';
  cmakeFlags = map (option: lib.cmakeBool option true) [
    "USING_FBDEV" "USING_GLES2" "USE_SYSTEM_LIBZIP" "USE_SYSTEM_SNAPPY"
  ] ++ map (option: lib.cmakeBool option false) [
    "USING_EGL" "VULKAN" "USE_VULKAN_DISPLAY_KHR" "USING_X11_VULKAN" "USE_WAYLAND_WSI"
    "USING_QT_UI" "HEADLESS" "UNITTEST" "USE_DISCORD" "USE_SYSTEM_FFMPEG"
  ] ++ [ (lib.cmakeBool "LIBRETRO" libretroCore) ];
  installPhase = if libretroCore then ''
    runHook preInstall
    install -Dm644 lib/ppsspp_libretro.so -t $out/lib/libretro
    runHook postInstall
  '' else ''
    runHook preInstall
    install -Dm755 PPSSPPSDL $out/bin/ppsspp
    mkdir -p $out/share/ppsspp
    cp -r assets $out/share/ppsspp/assets
    runHook postInstall
  '';
  meta = ppsspp.meta // { description = "PPSSPP for KMS/GLES handhelds"; };
}

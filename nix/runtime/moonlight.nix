# Moonlight Embedded (GameStream/Sunshine client) with SDL video and audio on
# the KMSDRM SDL2 build, software H.264 decoding and the gamepad through SDL.
# ROCKNIX runs it the same way on the H700 (`platform = sdl`).
#
# Everything the handheld does not use is off: X11, VDPAU/VA-API, PulseAudio,
# CEC and the vendor decoders. Avahi is dropped too: ghazal browses mDNS
# itself and always passes the host's address (moonlight-no-avahi.patch).
{ lib, stdenv, moonlight-embedded, cmake, pkg-config, sdl2, ffmpeg, libopus,
  libevdev, udev, curl, openssl, expat, libuuid }:
stdenv.mkDerivation {
  pname = "moonlight-embedded-h700";
  inherit (moonlight-embedded) version src;
  patches = [
    # Fullscreen at the panel's own mode, scaled and letterboxed to the stream.
    ./moonlight-sdl-fit-panel.patch
    # curl's default connect timeout is minutes: an offline host hung the menu.
    ./moonlight-connect-timeout.patch
    ./moonlight-no-avahi.patch
  ];
  strictDeps = true;
  enableParallelBuilding = true;
  # Built like the emulators: without nixpkgs' hardening flags.
  hardeningDisable = [ "all" ];
  nativeBuildInputs = [ cmake pkg-config ];
  buildInputs = [ sdl2 ffmpeg libopus libevdev udev curl openssl expat libuuid ];
  # No manual pages (they need pod2man), no sample configuration (ghazal
  # passes every option) and no bundled controller database (the profile
  # ships the H700 pad's mapping).
  postPatch = ''
    substituteInPlace CMakeLists.txt \
      --replace-fail 'add_subdirectory(docs)' "" \
      --replace-fail 'install(FILES ./third_party/SDL_GameControllerDB/gamecontrollerdb.txt DESTINATION ''${CMAKE_INSTALL_DATADIR}/moonlight)' "" \
      --replace-fail 'install(FILES moonlight.conf DESTINATION ''${CMAKE_INSTALL_SYSCONFDIR})' ""
  '';
  cmakeFlags = map (feature: lib.cmakeBool "ENABLE_${feature}" false) [ "X11" "CEC" "PULSE" ];
  meta = {
    description = "Moonlight for KMSDRM handhelds, software decoding";
    homepage = "https://github.com/moonlight-stream/moonlight-embedded";
    license = lib.licenses.gpl3Plus;
    mainProgram = "moonlight";
    platforms = lib.platforms.linux;
  };
}

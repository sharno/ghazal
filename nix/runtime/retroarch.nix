# RetroArch for a KMS handheld: GLES 3.1 on EGL/GBM, ALSA, udev input and the
# RGUI menu (retroarch.cfg selects these). nixpkgs' retroarch-bare also builds
# Qt, X11, Wayland, Vulkan, FFmpeg, PulseAudio and PipeWire support (~1.3 GB).
{ lib, stdenv, retroarch-bare, pkg-config, mesa, libdrm, alsa-lib, udev, zlib }:
stdenv.mkDerivation {
  pname = "retroarch-h700";
  inherit (retroarch-bare) version src;
  strictDeps = true;
  enableParallelBuilding = true;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ mesa libdrm alsa-lib udev zlib ];
  configureFlags = map (feature: "--enable-${feature}") [
    "kms" "egl" "opengles" "opengles3" "opengles3_1" "alsa" "udev" "zlib"
  ] ++ map (feature: "--disable-${feature}") [
    "builtinzlib" "opengl1" "vulkan" "x11" "wayland" "qt" "sdl" "sdl2" "ffmpeg" "freetype"
    "pulse" "pipewire" "jack" "oss" "al" "dbus" "systemd" "libusb" "v4l2" "discord"
    "slang" "glslang" "spirv_cross" "cg" "vg" "caca" "sixel" "crtswitchres"
    "materialui" "xmb" "ozone" "online_updater" "update_cores" "update_assets" "update_core_info"
  ];
  installPhase = ''
    runHook preInstall
    install -Dm755 retroarch $out/bin/retroarch
    runHook postInstall
  '';
  meta = retroarch-bare.meta // { description = "RetroArch for KMS/GLES handhelds"; };
}

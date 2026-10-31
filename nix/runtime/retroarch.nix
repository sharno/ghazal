# RetroArch for a KMS handheld: GLES 3.1 on EGL/GBM, ALSA, udev input and the
# RGUI menu (retroarch.cfg selects these). nixpkgs' retroarch-bare also builds
# Qt, X11, Wayland, Vulkan, FFmpeg, PulseAudio and PipeWire support (~1.3 GB).
#
# RetroAchievements and netplay need networking, TLS (RetroAchievements is
# HTTPS only; the built-in mbedTLS carries its own CA bundle, so no system
# certificates are read) and LAN discovery. The network command port that opens
# menu pages comes with networking. configure drops a feature it cannot build
# with only a notice, so they are enabled explicitly and checked in the binary.
{ lib, stdenv, retroarch-bare, pkg-config, mesa, libdrm, alsa-lib, udev, zlib }:
stdenv.mkDerivation {
  pname = "retroarch-h700";
  inherit (retroarch-bare) version src;
  strictDeps = true;
  enableParallelBuilding = true;
  # Built like ROCKNIX's, without nixpkgs' hardening flags (see rocknix-core.nix).
  hardeningDisable = [ "all" ];
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ mesa libdrm alsa-lib udev zlib ];
  configureFlags = map (feature: "--enable-${feature}") [
    "kms" "egl" "opengles" "opengles3" "opengles3_1" "alsa" "udev" "zlib"
    "networking" "ssl" "builtinmbedtls" "cheevos" "netplaydiscovery"
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
  # One string that only exists when each feature was compiled in: the
  # RetroAchievements settings, netplay's command line and LAN discovery, the
  # built-in CA bundle (TLS), and the network command interface.
  doInstallCheck = true;
  installCheckPhase = ''
    for needle in 'cheevos_enable' 'Host netplay as user 1' '[Discovery] Query received on LAN interface' \
        'Bundle of CA Root Certificates' 'network_cmd_enable'; do
      grep -aqF -- "$needle" $out/bin/retroarch || { echo "RetroArch was built without: $needle" >&2; exit 1; }
    done
  '';
  meta = retroarch-bare.meta // { description = "RetroArch for KMS/GLES handhelds"; };
}

# The device's userspace, built from the flake's nixpkgs for aarch64-linux.
#
# A process cannot mix two glibcs, so Mesa, RetroArch, PPSSPP and every core
# come from this one package set. Graphics and emulator packages are rebuilt
# without X11, Wayland, LLVM, Vulkan or FFmpeg: stock nixpkgs closures would
# not fit a 1 GiB OS slot. `tree` is the /usr and /etc layout the image builder
# copies onto the root filesystem; `closure` lists the store paths it needs.
{ pkgs, rocknix, profile }:
let
  inherit (pkgs) lib callPackage;
  mesa = callPackage ./mesa.nix { };
  sdl2 = callPackage ./sdl2.nix { inherit mesa rocknix; };
  # PPSSPP draws its interface text with it. Library only, without HarfBuzz.
  SDL2_ttf = pkgs.stdenv.mkDerivation {
    pname = "SDL2_ttf-h700";
    inherit (pkgs.SDL2_ttf) version src meta;
    strictDeps = true;
    nativeBuildInputs = [ pkgs.cmake pkgs.pkg-config ];
    buildInputs = [ sdl2 pkgs.freetype ];
    cmakeFlags = [ (lib.cmakeFeature "CMAKE_INSTALL_LIBDIR" "lib") (lib.cmakeFeature "CMAKE_INSTALL_INCLUDEDIR" "include") ]
      ++ map (option: lib.cmakeBool option false) [ "SDL2TTF_SAMPLES" "SDL2TTF_HARFBUZZ" "SDL2TTF_VENDORED" ];
  };
  retroarch = callPackage ./retroarch.nix { inherit mesa; };
  ffmpeg-h264 = callPackage ./ffmpeg.nix { };
  moonlight = callPackage ./moonlight.nix { inherit sdl2; ffmpeg = ffmpeg-h264; };
  decode-bench = callPackage ./decode-bench.nix { ffmpeg = ffmpeg-h264; };
  ppsspp = callPackage ./ppsspp.nix { inherit mesa sdl2 SDL2_ttf; };
  ppsspp-libretro = ppsspp.override { libretroCore = true; };
  cores = import ./cores.nix {
    rocknixCore = import ./rocknix-core.nix { inherit (pkgs) lib stdenv fetchurl fetchgit cmake; inherit rocknix; };
    inherit (pkgs) zlib libpng libzip;
    inherit mesa ppsspp-libretro;
  };

  # Only the tools Ghazal runs or keeps for SSH debugging.
  alsa-utils = pkgs.stdenv.mkDerivation {
    pname = "alsa-utils-h700";
    inherit (pkgs.alsa-utils) version src meta;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.alsa-lib ];
    configureFlags = map (feature: "--disable-${feature}") [
      "alsamixer" "alsaconf" "bat" "xmlto" "rst2man" "nls"
    ] ++ [ "--with-udev-rules-dir=$(out)/lib/udev/rules.d" ];
    installFlags = [ "ASOUND_STATE_DIR=$(TMPDIR)/dummy" ];
    postInstall = "rm -r $out/sbin/alsa-info.sh $out/lib/udev";
  };
  openssh = pkgs.openssh.override {
    withPAM = false; withFIDO = false; withSecurityKey = false; withLdns = false; withKerberos = false;
  };
  dbus = pkgs.dbus.override { x11Support = false; enableSystemd = false; };
  udev = pkgs.systemdMinimal;
  # fw_printenv/fw_setenv for the U-Boot A/B environment (config/updates/fw_env.config).
  fw-env = pkgs.runCommand "ghazal-fw-env" { } ''
    mkdir -p $out/bin
    cp ${pkgs.ubootTools}/bin/fw_printenv $out/bin/
    ln -s fw_printenv $out/bin/fw_setenv
  '';

  # Every `core = ` in frontend.toml, in a system or its alternatives, must name a built core.
  frontend = lib.importTOML (profile + "/frontend.toml");
  emulators = lib.concatMap (system: [ system ] ++ system.alternative or [ ]) frontend.system;
  missing = lib.filter (core: !cores ? ${core})
    (map (emulator: emulator.core) (lib.filter (emulator: emulator ? core) emulators));

  # Root filesystem path -> store path.
  links = {
    "usr/bin/retroarch" = "${retroarch}/bin/retroarch";
    "usr/bin/ppsspp" = "${ppsspp}/bin/ppsspp";
    "usr/bin/moonlight" = "${moonlight}/bin/moonlight";
    "usr/bin/syncthing" = "${pkgs.syncthing}/bin/syncthing";
    "usr/bin/amixer" = "${alsa-utils}/bin/amixer";
    "usr/bin/alsaucm" = "${alsa-utils}/bin/alsaucm";
    "usr/sbin/alsactl" = "${alsa-utils}/sbin/alsactl";
    "usr/bin/udevadm" = "${udev}/bin/udevadm";
    "usr/lib/systemd/systemd-udevd" = "${udev}/lib/systemd/systemd-udevd";
    "usr/bin/dbus-daemon" = "${dbus}/bin/dbus-daemon";
    "usr/lib/iwd" = "${pkgs.iwd}/libexec/iwd";
    "usr/bin/iwctl" = "${pkgs.iwd}/bin/iwctl";
    "usr/sbin/iw" = "${pkgs.iw}/bin/iw";
    "usr/sbin/blkid" = "${pkgs.util-linuxMinimal.bin}/bin/blkid";
    "usr/sbin/mkfs.exfat" = "${pkgs.exfatprogs}/bin/mkfs.exfat";
    "usr/sbin/fsck.exfat" = "${pkgs.exfatprogs}/bin/fsck.exfat";
    "usr/sbin/sshd" = "${openssh}/bin/sshd";
    "usr/bin/ssh-keygen" = "${openssh}/bin/ssh-keygen";
    "usr/bin/rauc" = "${pkgs.rauc}/bin/rauc";
    "usr/bin/curl" = "${pkgs.curl.bin}/bin/curl";
    "usr/bin/fw_printenv" = "${fw-env}/bin/fw_printenv";
    "usr/bin/fw_setenv" = "${fw-env}/bin/fw_setenv";
    "etc/dbus-1/system.d/iwd.conf" = "${pkgs.iwd}/share/dbus-1/system.d/iwd-dbus.conf";
    "etc/ssl/certs/ca-certificates.crt" = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  } // lib.mapAttrs' (name: path: lib.nameValuePair "usr/lib/libretro/${name}" path) cores;

  tree = pkgs.runCommand "ghazal-runtime-tree" { } ''
    ${lib.concatStrings (lib.mapAttrsToList (link: target: ''
      test -e ${target}
      mkdir -p "$(dirname "$out/${link}")"
      ln -s ${target} "$out/${link}"
    '') links)}
    # RetroArch's joypad profiles, with the H700 pad's own: it sends BTN_WEST
    # for X and BTN_NORTH for Y, unlike the generic Xbox ordering. RetroArch
    # only searches the joypad driver's folder (udev/), not the root.
    mkdir -p $out/usr/share/libretro
    cp -r ${pkgs.retroarch-joypad-autoconfig}/share/libretro/autoconfig $out/usr/share/libretro/
    chmod u+w $out/usr/share/libretro/autoconfig/udev
    cp ${profile + "/gamepad.cfg"} "$out/usr/share/libretro/autoconfig/udev/H700 Gamepad.cfg"
    # Moonlight looks for its controller mapping in /usr/share/moonlight.
    install -Dm644 ${profile + "/gamecontrollerdb.txt"} $out/usr/share/moonlight/gamecontrollerdb.txt
    # udev applies only Ghazal's rules (written by the image builder).
    mkdir -p $out/etc/udev/rules.d
    for rules in ${udev}/lib/udev/rules.d/*.rules; do
      ln -s /dev/null "$out/etc/udev/rules.d/$(basename "$rules")"
    done
  '';

  # Seeds for /var/lib/ghazal/ppsspp: ROCKNIX's H700 settings and the CWCheat
  # database; the image builder merges config/*/ppsspp over them.
  ppsspp-defaults = pkgs.runCommand "ghazal-ppsspp-defaults" { } ''
    source=${rocknix}/projects/ROCKNIX/packages/emulators/standalone/ppsspp-sa
    mkdir -p $out/PSP/SYSTEM
    cp -r $source/config/PSP/Cheats $out/PSP/
    chmod u+w $out/PSP/Cheats
    cp ${pkgs.fetchurl {
      url = "https://raw.githubusercontent.com/Saramagrean/CWCheat-Database-Plus-/7c9fe1ae71155626cea767aed53f968de9f4051f/cheat.db";
      hash = "sha256-3Mq9ch2XFrij9QbB9FOuV/qqGRTIf6rC/4+1qbv6K+w=";
    }} $out/PSP/Cheats/cheat.db
    cp $source/sources/H700/ppsspp.ini $out/PSP/SYSTEM/
  '';
in
assert lib.assertMsg (missing == [ ]) "frontend.toml names unbuilt cores: ${toString missing}";
{
  inherit mesa sdl2 retroarch ppsspp ppsspp-libretro moonlight decode-bench cores tree ppsspp-defaults;
  closure = pkgs.closureInfo { rootPaths = [ tree ]; };
}

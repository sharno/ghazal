# What PortMaster's ports expect a Linux handheld to provide, from the runtime's
# nixpkgs. Ports are bash scripts around glibc aarch64 binaries built for stock
# ROCKNIX, muOS and ArkOS: they name /lib/ld-linux-aarch64.so.1 as interpreter
# (linked in default.nix), call a handful of tools, and take SDL2, EGL, ALSA and
# friends from the system. Nothing here is on the default PATH or library path:
# `ghazal ports run` and PortMaster's control.txt (config/*/ports) add `bin` and
# `lib` for ports only. Keep the library list equal to SONAMES in src/ports/check.rs.
{ lib, runCommand, stdenv, busybox, python, util-linuxMinimal, squashfsTools, curl,
  mesa, sdl2, SDL2_ttf, SDL2_image, SDL2_mixer, alsa-lib, libdrm, udev, zlib, libpng }:
let
  # BusyBox applets beyond the base image's: PortMaster's scripts and its Python
  # code use these, and `env` is what `#!/usr/bin/env python3` needs.
  applets = [
    "env" "basename" "dirname" "readlink" "realpath" "touch" "pkill" "pgrep" "pidof" "killall"
    "unzip" "tar" "gzip" "gunzip" "zcat" "bunzip2" "bzcat" "unxz" "xzcat" "wget"
    "seq" "id" "whoami" "mktemp" "which" "nproc" "timeout" "expr" "xxd" "od" "rev" "install"
    "mountpoint" "nohup" "setsid" "tty" "clear" "stty" "cmp" "tac" "nl" "paste" "fold"
    "hostname" "lsusb" "sha1sum" "sha512sum" "base64" "chgrp" "unshare" "nsenter"
    # PortMaster's dialog talks through a fifo; ports parse options and do arithmetic.
    "mkfifo" "getopt" "printenv" "flock" "shuf" "rmdir" "yes" "truncate" "bc" "fuser" "lsof"
  ];
  # soname -> the package that has it.
  libraries = {
    "libSDL2-2.0.so.0" = sdl2;
    "libSDL2_image-2.0.so.0" = SDL2_image;
    "libSDL2_mixer-2.0.so.0" = SDL2_mixer;
    "libSDL2_ttf-2.0.so.0" = SDL2_ttf;
    "libEGL.so.1" = mesa;
    "libGLESv2.so.2" = mesa;
    "libgbm.so.1" = mesa;
    "libdrm.so.2" = libdrm;
    "libasound.so.2" = alsa-lib;
    "libudev.so.1" = udev;
    "libz.so.1" = zlib;
    "libpng16.so.16" = libpng;
    "libstdc++.so.6" = stdenv.cc.cc.lib;
    "libgcc_s.so.1" = stdenv.cc.cc.lib;
  };
in
runCommand "ghazal-ports" { } ''
  mkdir -p $out/bin $out/lib
  # A missing applet would otherwise show up as a port failing on the handheld.
  available=$'\n'"$(${busybox}/bin/busybox --list)"$'\n'
  for applet in ${toString applets}; do
    [[ "$available" == *$'\n'"$applet"$'\n'* ]] || { echo "busybox has no $applet applet"; exit 1; }
    ln -s ${busybox}/bin/busybox $out/bin/$applet
  done
  # Ports mount runtimes (squashfs files) with plain `mount`, which needs to
  # attach the loop device itself; util-linux's does, and BusyBox's would shadow it.
  for tool in mount umount losetup; do ln -s ${util-linuxMinimal.bin}/bin/$tool $out/bin/$tool; done
  ln -s ${squashfsTools}/bin/unsquashfs $out/bin/unsquashfs
  ln -s ${curl.bin}/bin/curl $out/bin/curl
  ln -s ${python}/bin/python3 $out/bin/python3
  ln -s python3 $out/bin/python
  ${lib.concatStrings (lib.mapAttrsToList (soname: package: ''
    test -e ${package}/lib/${soname} || { echo "${soname} is not in ${package}"; exit 1; }
    ln -s ${package}/lib/${soname} $out/lib/${soname}
  '') libraries)}
''

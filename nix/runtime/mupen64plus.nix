# Standalone Mupen64Plus from ROCKNIX's H700 recipes
# (packages/emulators/standalone/mupen64plus-sa): the core with its arm64
# dynarec, the console front end, SDL audio and input, the HLE RSP and the Rice
# video plugin, which is ROCKNIX's H700 default. Built for GLES and without the
# on-screen display, as ROCKNIX does.
{ lib, stdenv, pkg-config, rocknixPackage, sdl2, mesa, zlib, libpng, runCommand, writeShellScript, busybox }:
let
  part = name: rocknixPackage { path = "emulators/standalone/mupen64plus-sa/mupen64plus-sa-${name}"; };
  parts = lib.genAttrs [ "core" "ui-console" "audio-sdl" "input-sdl" "rsp-hle" "video-rice" ] part;
  # The make options ROCKNIX's configure_target exports for every part.
  options = [
    "HOST_CPU=aarch64" "NEW_DYNAREC=1" "USE_GLES=1" "VULKAN=0" "OSD=0" "V=1"
    "PREFIX=$out" "LIBDIR=$out/lib" "PLUGINDIR=$out/lib/mupen64plus" "SHAREDIR=$out/share/mupen64plus"
    "COREDIR=$out/lib/" "LDCONFIG=true"
  ];
  mupen64plus = stdenv.mkDerivation {
    pname = "mupen64plus-h700";
    version = builtins.substring 0 7 parts.core.rev;
    srcs = lib.mapAttrsToList (_: part: part.src) parts;
    sourceRoot = ".";
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ sdl2 mesa zlib libpng ];
    # Each part unpacks to <repository>-<revision>.
    postUnpack = lib.concatStrings (lib.mapAttrsToList (name: part: ''
      mv mupen64plus-${name}-${part.rev} ${name}
    '') parts);
    patchPhase = ''
      runHook prePatch
      ${lib.concatStrings (lib.mapAttrsToList (name: part: lib.concatMapStrings (patch: ''
        patch -d ${name} -p1 < ${patch}
      '') part.patches) parts)}
      sed -i 's/\-O[23]/-Ofast/' */projects/unix/Makefile
      runHook postPatch
    '';
    dontConfigure = true;
    env = {
      SDL_CFLAGS = "-I${lib.getDev sdl2}/include/SDL2 -D_REENTRANT";
      SDL_LDLIBS = "-lSDL2";
      # GCC 15 rejects what ROCKNIX's ui-console build only warns about.
      NIX_CFLAGS_COMPILE = "-Wno-error=incompatible-pointer-types";
    };
    buildPhase = ''
      runHook preBuild
      for part in ${toString (lib.attrNames parts)}; do
        make -C $part/projects/unix -j$NIX_BUILD_CORES all APIDIR=$PWD/core/src/api ${toString options}
      done
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      for part in ${toString (lib.attrNames parts)}; do
        make -C $part/projects/unix install APIDIR=$PWD/core/src/api ${toString options}
      done
      rm -rf $out/include $out/share/applications $out/share/icons $out/share/man
      runHook postInstall
    '';
  };
  # Mupen64Plus's data folder (`--datadir`): its ROM database and plugin
  # settings, with the handheld's pad first in the controller list, as ROCKNIX
  # ships it, ahead of upstream's entries for other controllers.
  data = runCommand "mupen64plus-data-h700" { } ''
    cp -r ${mupen64plus}/share/mupen64plus $out
    chmod u+w $out $out/InputAutoCfg.ini
    cat ${(part "input-sdl").dir}/config/H700/default.ini ${mupen64plus}/share/mupen64plus/InputAutoCfg.ini > $out/InputAutoCfg.ini
  '';
in
{
  inherit data;
  # Mupen64Plus loads a ROM file whole and cannot read archives: a zipped ROM
  # is unpacked into /tmp (RAM) first, as ROCKNIX's start script does.
  launcher = writeShellScript "mupen64plus" ''
    set -eu
    rom=''${!#}
    case $rom in
      *.zip|*.ZIP)
        dir=/tmp/mupen64plus
        ${busybox}/bin/busybox rm -rf "$dir"
        ${busybox}/bin/busybox mkdir -p "$dir"
        ${busybox}/bin/busybox unzip -q -o "$rom" -d "$dir"
        set -- "''${@:1:$#-1}" "$(${busybox}/bin/busybox find "$dir" -type f | ${busybox}/bin/busybox head -n 1)"
        ;;
    esac
    exec ${mupen64plus}/bin/mupen64plus "$@"
  '';
}

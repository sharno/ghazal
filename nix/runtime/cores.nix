# libretro cores by the file name frontend.toml uses, each built from
# ROCKNIX's H700 recipe (rocknix-core.nix). Options its package.mk sets in
# conditionals or shell functions are restated for the H700's aarch64 build
# (no TARGET_FPU), GLES preferred over desktop GL, no Vulkan.
{ rocknixCore, mesa, zlib, libpng, libzip, ppsspp-libretro }:
let
  core = args: "${rocknixCore args}/lib/libretro/${baseNameOf args.library}";
in
{
  "gambatte_libretro.so" = core { package = "gambatte-lr"; library = "gambatte_libretro.so"; };
  "mgba_libretro.so" = core { package = "mgba-lr"; builder = "cmake"; library = "mgba_libretro.so"; };
  "fceumm_libretro.so" = core { package = "fceumm-lr"; library = "fceumm_libretro.so"; };
  "snes9x_libretro.so" = core { package = "snes9x-lr"; library = "libretro/snes9x_libretro.so"; };
  "genesis_plus_gx_libretro.so" = core { package = "genesis-plus-gx-lr"; library = "genesis_plus_gx_libretro.so"; };
  "picodrive_libretro.so" = core {
    package = "picodrive-lr";
    hash = "sha256-fD3NHgPjbEpKNZQNJX4N6PYS+9pR405Szu/sqDDP+9s=";
    library = "picodrive_libretro.so";
  };
  "beetle_pce_fast_libretro.so" = core { package = "beetle-pce-fast-lr"; library = "mednafen_pce_fast_libretro.so"; };
  "beetle_ngp_libretro.so" = core { package = "beetle-ngp-lr"; library = "mednafen_ngp_libretro.so"; };
  "beetle_wswan_libretro.so" = core { package = "beetle-wswan-lr"; library = "mednafen_wswan_libretro.so"; };
  "beetle_vb_libretro.so" = core { package = "beetle-vb-lr"; library = "mednafen_vb_libretro.so"; };
  "stella_libretro.so" = core {
    package = "stella-lr";
    extraMakeFlags = [ "-C" "../src/os/libretro" "platform=aarch64" ];
    library = "src/os/libretro/stella_libretro.so";
  };
  "handy_libretro.so" = core { package = "handy-lr"; extraMakeFlags = [ "platform=emuelec" ]; library = "handy_libretro.so"; };
  "gw_libretro.so" = core { package = "gw-lr"; library = "gw_libretro.so"; };
  "bluemsx_libretro.so" = core { package = "bluemsx-lr"; library = "bluemsx_libretro.so"; };
  "fbneo_libretro.so" = core {
    package = "fbneo-lr";
    prePatch = ''
      sed -i "s|LDFLAGS += -static-libgcc -static-libstdc++|LDFLAGS += -static-libgcc|" src/burner/libretro/Makefile
    '';
    library = "src/burner/libretro/fbneo_libretro.so";
  };
  "mame2003_plus_libretro.so" = core { package = "mame2003-plus-lr"; library = "mame2003_plus_libretro.so"; };
  "pcsx_rearmed_libretro.so" = core {
    package = "pcsx_rearmed-lr";
    makeFlags = [ "-f" "Makefile.libretro" "-C" ".." "platform=H700" ];
    prePatch = "sed -i 's/\\-O[23]/-Ofast/' Makefile";
    library = "pcsx_rearmed_libretro.so";
  };
  # Without its JIT: ROCKNIX's build has none, and its memory setup crashes.
  "melonds_libretro.so" = core {
    package = "melonds-lr";
    preBuild = "rm -f CMakeLists.txt";
    buildInputs = [ zlib ];
    library = "melonds_libretro.so";
  };
  "mupen64plus_next_libretro.so" = core {
    package = "mupen64plus-nx-lr";
    extraMakeFlags = [ "GL_LIB=-lGLESv2" "GLES3=1" "platform=H700" ];
    prePatch = ''
      for source in mupen64plus-rsp-paraLLEl/rsp_disasm.cpp mupen64plus-rsp-paraLLEl/rsp_disasm.hpp; do
        sed -i '/include <string>/a #include <cstdint>' $source
      done
      sed -e "s|^GIT_VERSION ?.*$|GIT_VERSION := \" ''${PKG_VERSION:0:7}\"|" -i Makefile
      sed -i 's/\-O[23]/-Ofast/' Makefile
    '';
    cflags = [ "-DHAVE_UNISTD_H" "-Wno-error=incompatible-pointer-types" ];
    cxxflags = [ "-mno-outline-atomics" ];
    buildInputs = [ mesa zlib libpng ];
    library = "mupen64plus_next_libretro.so";
  };
  "flycast_libretro.so" = core {
    package = "flycast-lr";
    hash = "sha256-/QfEDbodvtrfxe2qEqselWFyv6Lx77Z3fJOZhgwMAIk=";
    builder = "cmake";
    extraCmakeFlags = [ "-DUSE_OPENGL=ON" "-DUSE_GLES=ON" "-DUSE_VULKAN=OFF" "-Wno-dev" "-DLIBRETRO=ON" "-DWITH_SYSTEM_ZLIB=ON" "-DUSE_OPENMP=ON" ];
    # ROCKNIX names its options flycast_*, so settings saved under ROCKNIX apply.
    prePatch = ''
      sed -i 's/"reicast"/"flycast"/g' shell/libretro/libretro_core_option_defines.h
      sed -i 's/\-O[23]/-Ofast/' CMakeLists.txt
    '';
    buildInputs = [ mesa zlib libzip ];
    library = "flycast_libretro.so";
  };
  "yabasanshiro_libretro.so" = core {
    package = "yabasanshiro-lr";
    hash = "sha256-Fo5s2GyEtCEbKpiXXyJ0Aki5ZQMbYTnCIMIhVFeGi1o=";
    extraMakeFlags = [ "-C" "yabause/src/libretro" "FORCE_GLES=1" "platform=rockpro64" "HAVE_NEON=0" ];
    prePatch = "sed -i 's/\\-O[23]/-Ofast -ffast-math/' yabause/src/libretro/Makefile";
    # GCC 15 also errors on what ROCKNIX's GCC 16 build accepts.
    cflags = [ "-Wno-implicit-function-declaration" "-Wno-error=incompatible-pointer-types" "-Wno-error=int-conversion"
      "-flto" "-fipa-pta" "-mno-outline-atomics" ];
    cxxflags = [ "-flto" "-fipa-pta" "-mno-outline-atomics" ];
    ldflags = [ "-flto" "-fipa-pta" ];
    buildInputs = [ mesa zlib ];
    library = "yabause/src/libretro/yabasanshiro_libretro.so";
  };
  "ppsspp_libretro.so" = "${ppsspp-libretro}/lib/libretro/ppsspp_libretro.so";
}

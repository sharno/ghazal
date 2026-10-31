#!/bin/bash
# Off-device proof that PortMaster ports can run on Ghazal's runtime (docs/ports.md).
#
# Runs in an aarch64 Linux container, as root, with the shared Nix store, the
# runtime tree and a `ghazal` device binary (cargo build --release --target
# aarch64-unknown-linux-musl; it is static, so it runs there as it does on the
# handheld). A real PortMaster folder (its tools and Python code) makes the test
# thorough; without PM_SAMPLE only what the runtime alone can show is checked.
#
#   docker run --rm --privileged -v oxmux-nix:/nix -v "$PWD":/repo:ro \
#     -v /path/to/PortMaster:/sample/PortMaster:ro \
#     -e GHAZAL=/repo/target/aarch64-unknown-linux-musl/release/ghazal \
#     -e TREE=$(nix build --no-link --print-out-paths .#runtime) \
#     -e SDL2=<store path of the runtime's sdl2> -e BUILD_TOOLS=<store path of a profile with gcc and patchelf> \
#     -e PM_SAMPLE=/sample/PortMaster nixos/nix:latest \
#     $TREE/usr/bin/bash /repo/scripts/test-ports.sh
#
# Needs: GHAZAL, TREE, SDL2, BUILD_TOOLS. Optional: PM_SAMPLE, SQUASHFS_TOOLS.
set -u
: "${GHAZAL:?}" "${TREE:?}" "${SDL2:?}" "${BUILD_TOOLS:?}"
REPO=${REPO:-/repo}
PORTS_BIN=$TREE/usr/lib/ghazal/ports/bin
export PATH="$PORTS_BIN:$BUILD_TOOLS/bin:$PATH"

failures=0
pass() { echo "ok    $1"; }
fail() { echo "FAIL  $1"; failures=$((failures + 1)); }
check() { # check DESCRIPTION COMMAND...
  local what=$1; shift
  if "$@" >/tmp/check.out 2>&1; then pass "$what"; else fail "$what: $(tail -n 3 /tmp/check.out | tr '\n' ' ')"; fi
}
contains() { grep -q -- "$2" "$1"; }

# --- The root filesystem the image builder would make, as far as ports see it.
mkdir -p /lib /usr/bin /usr/lib /roms/ports /etc/ghazal /run/ghazal /var/log/ghazal /mnt/test
# /usr/bin holds the BusyBox applet links the image builder makes (read from its source).
BUSYBOX=$(readlink -f "$PORTS_BIN/env")
listed=0
while IFS= read -r line; do
  case $line in
    *"for applet in ["*) listed=1; continue ;;
    *"] {"*) listed=0 ;;
  esac
  if [ $listed = 1 ]; then applet=${line//[ \",]/}; [ -n "$applet" ] && ln -sf "$BUSYBOX" "/usr/bin/$applet"; fi
done < "$REPO/tools/image-builder/src/main.rs"
ln -sf "$TREE/usr/lib/ld-linux-aarch64.so.1" /lib/ld-linux-aarch64.so.1
ln -sf "$TREE/usr/lib/libc.so.6" /usr/lib/libc.so.6
ln -sf "$TREE/usr/bin/bash" /usr/bin/bash
ln -sf "$TREE/usr/bin/env" /usr/bin/env
ln -sfn "$TREE/usr/lib/ghazal" /usr/lib/ghazal
for lib in "$TREE"/usr/lib/libSDL2*.so.0; do ln -sf "$lib" "/usr/lib/$(basename "$lib")"; done
rm -rf /etc/ghazal/ports && cp -r "$REPO/config/rg40xxv-rocknix/ports" /etc/ghazal/ports

WORK=/work
rm -rf $WORK && mkdir -p $WORK/ROMS/PORTS $WORK/home /test/etc
cat > /test/etc/frontend.toml <<EOF
rom_roots = ["$WORK/ROMS"]
[retroarch]
bin = "retroarch"
core_dirs = []
[ports]
home = "$WORK/home"
log = "$WORK/ports.log"
EOF
ghazal() { "$GHAZAL" --config-dir /test/etc "$@"; }

# --- A port binary as PortMaster ports ship them: glibc, FHS interpreter, no RPATH.
cat > $WORK/hello.c <<'EOF'
#include <SDL2/SDL.h>
#include <stdio.h>
int main(void) {
  SDL_version v; SDL_GetVersion(&v);
  printf("SDL %d.%d.%d\n", v.major, v.minor, v.patch);
  if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_JOYSTICK) != 0) { printf("SDL_Init: %s\n", SDL_GetError()); return 2; }
  printf("video driver: %s\n", SDL_GetCurrentVideoDriver());
  SDL_Quit();
  puts("port-binary-ok");
  return 0;
}
EOF
PORT=$WORK/ROMS/PORTS/helloport
mkdir -p $PORT
gcc -o $PORT/hello $WORK/hello.c -I"$SDL2/include" -L"$SDL2/lib" -lSDL2 -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1 \
  && patchelf --remove-rpath $PORT/hello
check "the test port binary names the FHS loader and has no RPATH" sh -c "patchelf --print-interpreter $PORT/hello | grep -qx /lib/ld-linux-aarch64.so.1 && [ -z \"\$(patchelf --print-rpath $PORT/hello)\" ]"

# --- PortMaster's folder: a real one when given, else a stand-in with its file names.
PM=$WORK/ROMS/PORTS/PortMaster
if [ -n "${PM_SAMPLE:-}" ]; then
  cp -r "$PM_SAMPLE" $PM
else
  mkdir -p $PM
  for f in device_info.txt funcs.txt PortMasterDialog.txt; do echo "# stand-in" > $PM/$f; done
  echo "2000.01.01-0000" > $PM/version
fi
chmod -R u+w $PM

# --- A port script written like the ones PortMaster generates.
cat > $WORK/ROMS/PORTS/HelloPort.sh <<'EOF'
#!/bin/bash
XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
if [ -d "/opt/system/Tools/PortMaster/" ]; then
  controlfolder="/opt/system/Tools/PortMaster"
elif [ -d "/opt/tools/PortMaster/" ]; then
  controlfolder="/opt/tools/PortMaster"
elif [ -d "$XDG_DATA_HOME/PortMaster/" ]; then
  controlfolder="$XDG_DATA_HOME/PortMaster"
else
  controlfolder="/roms/ports/PortMaster"
fi
source $controlfolder/control.txt
[ -f "${controlfolder}/mod_${CFW_NAME}.txt" ] && source "${controlfolder}/mod_${CFW_NAME}.txt"
get_controls
GAMEDIR="/$directory/ports/helloport"
cd $GAMEDIR
> "$GAMEDIR/log.txt" && exec > >(tee "$GAMEDIR/log.txt") 2>&1
$ESUDO chmod +x ./hello
export LD_LIBRARY_PATH="$GAMEDIR/libs.${DEVICE_ARCH}:$LD_LIBRARY_PATH"
export SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
echo "CFW=$CFW_NAME directory=$directory ESUDO=[$ESUDO] ARCH=$DEVICE_ARCH SIZE=${DISPLAY_WIDTH}x${DISPLAY_HEIGHT} STICKS=$ANALOGSTICKS"
echo "GPTOKEYB=$GPTOKEYB"
echo "PATH=$PATH"
echo "python=$(command -v python3) pkill=$(command -v pkill) unzip=$(command -v unzip)"
echo "tty0=$(stat -c %t:%T /dev/tty0) cwd=$PWD script=$0"
pm_platform_helper "$GAMEDIR/hello"
./hello
status=$?
pm_finish
exit $status
EOF
chmod +x $WORK/ROMS/PORTS/HelloPort.sh

# --- ghazal ports run
ghazal ports run $WORK/ROMS/PORTS/HelloPort.sh > $WORK/run.out 2>&1
echo "ghazal ports run exited $?"
cat $WORK/run.out | head -40
check "a PortMaster-style script runs under ghazal ports run" contains $WORK/run.out "port-binary-ok"
check "the port saw /roms/ports as its directory, with Ghazal's control.txt" contains $WORK/run.out "CFW=Ghazal directory=roms ESUDO=\[\]"
check "control.txt replaced PortMaster's own" cmp /etc/ghazal/ports/control.txt $PM/control.txt
check "SDL2 loaded from the compat directory by the FHS loader" contains $WORK/run.out "^SDL 2\."
check "gptokeyb is wired" contains $WORK/run.out "GPTOKEYB= */roms/ports/PortMaster/gptokeyb -1"
check "ports tools come first on PATH" contains $WORK/run.out "python=/usr/lib/ghazal/ports/bin/python3 pkill=/usr/lib/ghazal/ports/bin/pkill"
check "virtual consoles are /dev/null inside the namespace" contains $WORK/run.out "tty0=1:3 "
check "the port's own log is written through tee" contains $PORT/log.txt "port-binary-ok"
check "the ports folder is not bound outside the port's namespace" sh -c '[ -z "$(ls /roms/ports)" ]'

# --- ghazal ports check: the parts that do not depend on the handheld's hardware.
ghazal ports check > $WORK/check.out 2>&1
for item in loader bash env "tool python3" "tool unsquashfs" libraries python pysdl2 squashfs namespace; do
  check "ports check: $item" contains $WORK/check.out "^ok    $item:"
done

# --- Quick failure is reported; a long-running port's status is not.
cat > $WORK/ROMS/PORTS/Broken.sh <<'EOF'
#!/bin/bash
echo "cannot start"
exit 3
EOF
ghazal ports run $WORK/ROMS/PORTS/Broken.sh > $WORK/broken.out 2>&1; code=$?
check "an immediate failure keeps its exit status" [ $code -eq 3 ]
check "and names itself for the menu" contains $WORK/broken.out "^\[ERROR\] Broken.sh stopped"

# --- Menu + Start: TERM reaches the script, its children and gptokeyb-like helpers.
cat > $WORK/ROMS/PORTS/Sleepy.sh <<'EOF'
#!/bin/bash
sleep 300 &
sleep 301 &
echo started
sleep 302
EOF
"$GHAZAL" --config-dir /test/etc ports run $WORK/ROMS/PORTS/Sleepy.sh > $WORK/sleepy.out 2>&1 &
launcher=$!
sleep 2
kill -TERM $launcher
start=$(date +%s)
wait $launcher; code=$?
elapsed=$(( $(date +%s) - start ))
check "TERM ends the launcher quickly with status 0 (status $code after $elapsed s)" sh -c "[ $code -eq 0 ] && [ $elapsed -le 4 ]"
check "no helper survives it" sh -c '! pgrep -f "sleep 30[012]" >/dev/null'

# --- Runtimes: squashfs images are mounted with plain mount (loop device).
if [ -n "${SQUASHFS_TOOLS:-}" ]; then
  mkdir -p $WORK/rt-src && echo "inside-squashfs" > $WORK/rt-src/marker
  "$SQUASHFS_TOOLS/bin/mksquashfs" $WORK/rt-src $WORK/ROMS/PORTS/PortMaster/rt.squashfs -comp zstd -quiet -noappend >/dev/null
  cat > $WORK/ROMS/PORTS/Mounter.sh <<'EOF'
#!/bin/bash
mkdir -p /tmp/rt
mount /roms/ports/PortMaster/rt.squashfs /tmp/rt && cat /tmp/rt/marker
umount /tmp/rt
EOF
  ghazal ports run $WORK/ROMS/PORTS/Mounter.sh > $WORK/mount.out 2>&1
  check "a port mounts a squashfs runtime with plain mount" contains $WORK/mount.out "inside-squashfs"
else
  echo "skip  squashfs mount (SQUASHFS_TOOLS unset)"
fi

# --- PortMaster's own tools and Python code against the runtime.
if [ -n "${PM_SAMPLE:-}" ]; then
  export LD_LIBRARY_PATH="$TREE/usr/lib/ghazal/ports/lib" SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
  for tool in gptokeyb gptokeyb2 xdelta3 innoextract.aarch64 sdl2imgshow.aarch64 sdl_resolution.aarch64 7zzs.aarch64; do
    if [ -e $PM/$tool ]; then
      chmod +x $PM/$tool
      check "$tool resolves its libraries" sh -c "! /lib/ld-linux-aarch64.so.1 --list $PM/$tool 2>&1 | grep -q 'not found'"
    fi
  done
  check "ghazal ports ldd reports a loadable binary" ghazal ports ldd $PM/gptokeyb
  check "xdelta3 runs" sh -c "$PM/xdelta3 -V 2>&1 | grep -qi xdelta"
  check "innoextract runs" sh -c "$PM/innoextract.aarch64 --version 2>&1 | grep -qi innoextract"
  check "sdl_resolution initializes SDL with the dummy driver" sh -c "$PM/sdl_resolution.aarch64 2>&1 | grep -qa 'Current'"
  if [ -f $PM/pylibs.zip ]; then
    check "PortMaster's Python code unpacks (zipfile)" python3 -c "import zipfile; zipfile.ZipFile('$PM/pylibs.zip').extractall('$PM')"
  fi
  # PortMaster launcher sets this itself, to the standard place.
  export PYSDL2_DLL_PATH=/usr/lib PYTHONPATH="$PM/pylibs:$PM/exlibs"
  check "python has what PortMaster needs" python3 -c "import ctypes, ssl, zlib, lzma, bz2, json, zipfile, tarfile, gettext, hashlib, xml.etree.ElementTree"
  check "harbourmaster and the GUI modules import" python3 -c "import harbourmaster, pugscene, pugtheme, pySDL2gui, sdl2, sdl2.ext, sdl2.sdlmixer, sdl2.sdlttf, sdl2.sdlimage"
  BG=$PM/pylibs/default_theme/buttons.png
  check "SDL2, SDL2_image, SDL2_mixer and SDL2_ttf work through pysdl2" python3 - <<EOF
import sdl2, sdl2.sdlimage as img, sdl2.sdlmixer as mix, sdl2.sdlttf as ttf
assert sdl2.SDL_Init(sdl2.SDL_INIT_VIDEO | sdl2.SDL_INIT_AUDIO) == 0, sdl2.SDL_GetError()
assert img.IMG_Init(img.IMG_INIT_PNG | img.IMG_INIT_JPG) & img.IMG_INIT_PNG
surface = img.IMG_Load(b"$BG")
assert surface, img.IMG_GetError()
assert ttf.TTF_Init() == 0
assert mix.Mix_OpenAudio(44100, mix.MIX_DEFAULT_FORMAT, 2, 1024) == 0, mix.Mix_GetError()
print("pysdl2-ok")
EOF
fi

echo
if [ $failures -eq 0 ]; then echo "all checks passed"; else echo "$failures checks failed"; exit 1; fi

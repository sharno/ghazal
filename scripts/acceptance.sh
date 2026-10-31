#!/usr/bin/env bash
# Release acceptance on a real handheld, run from your computer over SSH.
#
#   scripts/acceptance.sh [options] [GAMES_FILE]
#
# GAMES_FILE lists one ROM path per line, as the handheld's menu shows it
# (scripts/acceptance-games.example). Keep your own list out of the repository.
# Without it only the system checks run. A running game is stopped first.
#
# Options:
#   --ssh COMMAND      runs a shell command on the handheld (default: $GHAZAL_SSH,
#                      else "ssh root@${GHAZAL_HOST:-ghazal}")
#   --max-boot SECONDS power-on to first frame limit (default 3.5; the budget is ~2.95 s)
#   --evidence DIR     copy the frames and the emulator's log here
#   -h, --help
#
# Every game is launched through the menu itself: the script computes the button
# presses from the menu's own library scan (`ghazal observe plan|launch`) and
# injects them through the real gamepad. RetroArch runs through a wrapper bound
# over its binary for the run, so saves, states and core options go to a scratch
# directory under /tmp and never touch the data card; RetroArch silently falls
# back to its defaults when those directories are missing, so the script makes them.
# PPSSPP keeps its fixed save paths; the script only launches and quits it.
# The wrapper is removed on exit. Exit status 1 when any check fails.
set -uo pipefail

G=/opt/ghazal/ghazal
SCRATCH=/tmp/ghazal-acceptance
MIN_COLORS=16
SSH_CMD=${GHAZAL_SSH:-"ssh -o ConnectTimeout=8 -o ServerAliveInterval=15 root@${GHAZAL_HOST:-ghazal}"}
MAX_BOOT=3.5
EVIDENCE=""
GAMES_FILE=""

usage() { sed -n '2,/^set -uo/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --ssh) SSH_CMD=$2; shift 2 ;;
    --max-boot) MAX_BOOT=$2; shift 2 ;;
    --evidence) EVIDENCE=$2; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) GAMES_FILE=$1; shift ;;
  esac
done
if [ -n "$GAMES_FILE" ] && [ ! -r "$GAMES_FILE" ]; then
  echo "cannot read $GAMES_FILE" >&2
  exit 2
fi
[ -z "$EVIDENCE" ] || mkdir -p "$EVIDENCE"

# --- helpers ------------------------------------------------------------------

# Runs a command on the handheld. The command is one string for its shell;
# ssh must not read the caller's stdin (a games file is read in a loop).
dev() { $SSH_CMD "$1" </dev/null; }

# Single-quotes a value for the handheld's shell.
q() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

ROWS=()
FAILURES=0
record() { # STATUS GAME CHECK DETAIL
  ROWS+=("$1|$2|$3|$4")
  [ "$1" != FAIL ] || FAILURES=$((FAILURES + 1))
  printf '%-5s %-34.34s %-42.42s %s\n' "$1" "$2" "$3" "$4"
}

# Waits on the handheld until a shell condition holds: SECONDS CONDITION.
wait_until() { dev "i=0; while [ \$i -lt $1 ]; do if { $2; } >/dev/null 2>&1; then exit 0; fi; i=\$((i + 1)); sleep 1; done; exit 1"; }

# Copies a file from the handheld into the evidence folder: REMOTE_COMMAND LOCAL_NAME.
evidence() {
  [ -z "$EVIDENCE" ] || dev "$1" >"$EVIDENCE/$2" 2>/dev/null || true
}

# Captures the screen and prints its distinct colour count; nothing on failure.
# The file name is random because this runs in a subshell.
frame() { # LABEL
  local file="$SCRATCH/frame-$RANDOM$RANDOM.ppm" out
  out=$(dev "$G observe frame $file" 2>&1) || return 1
  evidence "cat $file" "$1.ppm"
  dev "rm -f $file" || true
  sed -n 's/^distinct_colors=//p' <<<"$out"
}

# Polls the screen until it shows at least MIN_COLORS colours: SECONDS LABEL.
frame_with_content() {
  local deadline=$((SECONDS + $1)) colors=""
  while [ $SECONDS -lt $deadline ]; do
    colors=$(frame "$2") && [ "${colors:-0}" -ge "$MIN_COLORS" ] && { echo "$colors"; return 0; }
    sleep 2
  done
  echo "${colors:-no frame}"
  return 1
}

plan_get() { sed -n "s/^$1=//p" <<<"$PLAN" | head -n 1; }

# Stops any running game and waits for the menu.
wait_for_menu() {
  dev "[ ! -s /run/ghazal/emulator.toml ]" || { dev "$G ctl stop-emulator" >/dev/null 2>&1 || true; sleep 3; }
  wait_until 20 "[ ! -s /run/ghazal/emulator.toml ]"
}

cleanup() {
  trap - EXIT
  echo
  echo "Cleaning up the handheld..."
  dev "[ -s /run/ghazal/emulator.toml ] && $G ctl stop-emulator; real=\$(busybox readlink -f /usr/bin/retroarch); while grep -q \" \$real \" /proc/mounts; do umount \"\$real\"; done; grep -q ' $SCRATCH/real ' /proc/mounts && umount $SCRATCH/real; rm -rf $SCRATCH; true" >/dev/null 2>&1 || true
}

# --- system checks ------------------------------------------------------------

check_system() {
  local info version device slot order left_a left_b booted_left first t out colors
  info=$(dev 'cat /etc/ghazal/image.json') || { record FAIL system "reach the handheld over SSH" "$SSH_CMD failed"; return 1; }
  version=$(sed -n 's/.*"version": "\(.*\)".*/\1/p' <<<"$info")
  device=$(sed -n 's/.*"id": "\(.*\)".*/\1/p' <<<"$info" | head -n 1)
  record PASS system "reach the handheld over SSH" "ghazal $version on $device"

  slot=$(dev 'cat /proc/cmdline' | tr ' ' '\n' | sed -n 's/^rauc.slot=//p')
  out=$(dev 'fw_printenv BOOT_ORDER BOOT_A_LEFT BOOT_B_LEFT')
  order=$(sed -n 's/^BOOT_ORDER=//p' <<<"$out")
  left_a=$(sed -n 's/^BOOT_A_LEFT=//p' <<<"$out")
  left_b=$(sed -n 's/^BOOT_B_LEFT=//p' <<<"$out")
  booted_left=$left_a
  [ "$slot" != B ] || booted_left=$left_b
  first=${order%% *}
  if [ -n "$slot" ] && [ "$first" = "$slot" ] && [ "$booted_left" = 3 ]; then
    record PASS system "slot confirmed healthy, no fallback" "slot $slot, order '$order', attempts left A=$left_a B=$left_b"
  else
    record FAIL system "slot confirmed healthy, no fallback" "slot '$slot', order '$order', attempts left A=$left_a B=$left_b (want the booted slot first with 3 left)"
  fi

  out=$(dev "$G observe boot" 2>&1)
  t=$(sed -n 's/^power_on_to_first_frame_seconds = //p' <<<"$out")
  if [ -n "$t" ] && awk -v t="$t" -v m="$MAX_BOOT" 'BEGIN { exit !(t <= m) }'; then
    record PASS system "power-on to the menu's first frame" "$t s (limit $MAX_BOOT s)"
  else
    record FAIL system "power-on to the menu's first frame" "${t:-unreadable} s (limit $MAX_BOOT s); measure on a boot after a clean shutdown"
  fi

  dev "mkdir -p $SCRATCH" || true
  wait_for_menu || true
  if colors=$(frame_with_content 20 menu); then
    record PASS system "menu is drawn" "$colors distinct colours"
  else
    record FAIL system "menu is drawn" "frame has $colors distinct colours (want $MIN_COLORS+)"
  fi

  if out=$(dev "$G ctl status" 2>&1); then
    record PASS system "daemon answers" "$out"
  else
    record FAIL system "daemon answers" "$out"
  fi

  if [ "$(dev "busybox dmesg | busybox grep -c -E 'Oops|BUG:|Kernel panic'")" = 0 ]; then
    record PASS system "kernel log has no oops or BUG" "dmesg is clean"
  else
    record FAIL system "kernel log has no oops or BUG" "$(dev "busybox dmesg | busybox grep -E 'Oops|BUG:|Kernel panic' | busybox head -n 2")"
  fi
}

# --- games --------------------------------------------------------------------

# Binds a wrapper over RetroArch so the menu's launch uses scratch save paths.
install_wrapper() {
  $SSH_CMD "sh -s" <<REMOTE
set -e
real=\$(busybox readlink -f /usr/bin/retroarch)
mkdir -p $SCRATCH
# A run that died leaves its binds behind.
while grep -q " \$real " /proc/mounts; do umount "\$real"; done
if grep -q ' $SCRATCH/real ' /proc/mounts; then umount $SCRATCH/real; fi
: > $SCRATCH/real
mount --bind "\$real" $SCRATCH/real
cat > $SCRATCH/wrapper <<'WRAPPER'
#!/bin/sh
# Installed by scripts/acceptance.sh: the real RetroArch with scratch save locations.
exec $SCRATCH/real "\$@" --appendconfig $SCRATCH/scratch.cfg
WRAPPER
chmod 755 $SCRATCH/wrapper
mount --bind $SCRATCH/wrapper "\$real"
REMOTE
}

# A fresh scratch area for one game; the wrapper's `--appendconfig` points at it.
scratch_for() { # DIRECTORY
  $SSH_CMD "sh -s" <<REMOTE
set -e
mkdir -p $1/saves $1/states
cat > $SCRATCH/scratch.cfg <<CFG
config_save_on_exit = "false"
savefile_directory = "$1/saves"
savestate_directory = "$1/states"
core_options_path = "$1/core-options.cfg"
CFG
REMOTE
}

GAME_NUMBER=0

run_game() { # ROM
  local rom=$1 label directory log offset launch started colors pcm policy_dir governor min_khz pid out
  GAME_NUMBER=$((GAME_NUMBER + 1))
  # The library scan reports hidden systems on stderr; keep that out of the key=value lines.
  PLAN=$(dev "$G observe plan $(q "$rom") 2>$SCRATCH/plan.err") \
    || { record FAIL "$(basename "$rom")" "find the game in the menu" "$(dev "busybox tail -n 1 $SCRATCH/plan.err")"; return; }
  label="$(plan_get system): $(basename "$rom")"
  launch=$(plan_get launch)
  log=$(plan_get log)
  policy_dir=$(plan_get cpu_policy_dir)
  directory=$SCRATCH/game-$GAME_NUMBER

  wait_for_menu || { record FAIL "$label" "start from the menu" "an emulator is still running"; return; }
  if [ "$launch" = retroarch ]; then
    scratch_for "$directory" || { record FAIL "$label" "make scratch save directories" "could not create $directory"; return; }
  fi
  offset=$(dev "busybox wc -c < $(q "${log:-/dev/null}")" 2>/dev/null || echo 0)
  started=$SECONDS
  if ! out=$(dev "$G observe launch $(q "$rom")" 2>&1); then
    record FAIL "$label" "launch through the menu" "$out"
    return
  fi
  if ! wait_until 60 "[ -s /run/ghazal/emulator.toml ]"; then
    record FAIL "$label" "launch through the menu" "no emulator started; the menu path was: $(plan_get buttons)"
    return
  fi
  record PASS "$label" "launch through the menu" "emulator running after $((SECONDS - started)) s"
  pid=$(dev "sed -n 's/^pid = //p' /run/ghazal/emulator.toml")

  # The gamepad: RetroArch must configure it from the shipped profile. A standalone
  # emulator has no such log line; it must at least hold the pad's device open.
  if [ "$launch" = retroarch ]; then
    # The log rolls over at 4 MiB: a shorter file than before means a fresh one.
    [ "$(dev "busybox wc -c < $(q "$log")" 2>/dev/null || echo 0)" -ge "$offset" ] || offset=0
    local since
    since="busybox tail -c +$((offset + 1)) $(q "$log")"
    if wait_until 30 "$since | busybox grep -q 'configured in port'" && [ "$(dev "$since | busybox grep -c 'not configured'")" = 0 ]; then
      record PASS "$label" "gamepad configured by RetroArch" "$(dev "$since | busybox grep 'configured in port' | busybox head -n 1")"
    else
      record FAIL "$label" "gamepad configured by RetroArch" "$(dev "$since | busybox grep -E 'not configured|Autoconf' | busybox head -n 2")"
    fi
  else
    if wait_until 60 "ls -l /proc/$pid/fd | busybox grep -q /dev/input/event"; then
      record PASS "$label" "gamepad opened by the emulator" "pid $pid holds an input device"
    else
      record FAIL "$label" "gamepad opened by the emulator" "pid $pid has no /dev/input/event* open"
    fi
  fi

  # Audio: the PCM must be running, not merely opened.
  pcm=/proc/asound/card$(plan_get audio_card)/pcm0p/sub0/status
  if wait_until 60 "busybox grep -q 'state: RUNNING' $pcm"; then
    record PASS "$label" "audio PCM RUNNING" "$pcm"
  else
    record FAIL "$label" "audio PCM RUNNING" "$(dev "busybox head -n 3 $pcm" 2>&1 | tr '\n' ' ')"
  fi

  if colors=$(frame_with_content 60 "game-$GAME_NUMBER"); then
    record PASS "$label" "game frame is on screen" "$colors distinct colours"
  else
    record FAIL "$label" "game frame is on screen" "$colors distinct colours (want $MIN_COLORS+)"
  fi

  # The clock policy for the system's `cpu` load (frontend.toml, device.toml [cpu]).
  if [ -n "$policy_dir" ]; then
    governor=$(dev "cat $policy_dir/scaling_governor")
    min_khz=$(dev "cat $policy_dir/scaling_min_freq")
    if [ "$governor" = "$(plan_get game_governor)" ] && [ "$min_khz" = "$(plan_get game_min_khz)" ]; then
      record PASS "$label" "CPU policy matches load '$(plan_get cpu_load)'" "$governor, floor $min_khz kHz"
    else
      record FAIL "$label" "CPU policy matches load '$(plan_get cpu_load)'" "$governor/$min_khz, want $(plan_get game_governor)/$(plan_get game_min_khz)"
    fi
  fi

  if [ "$launch" = retroarch ]; then
    # Menu + R1 and Menu + L1 are RetroArch's save and load hotkeys on this pad.
    local states="busybox find $directory/states -type f | busybox grep -v '\.auto\$'"
    dev "$G observe press menu+r1" >/dev/null 2>&1
    if wait_until 15 "$states | busybox grep -q ."; then
      record PASS "$label" "Menu + R1 saves a state (scratch)" "$(dev "$states | busybox head -n 1")"
    else
      record FAIL "$label" "Menu + R1 saves a state (scratch)" "no state file under $directory/states; does the core support states?"
    fi
    dev "$G observe press menu+l1" >/dev/null 2>&1
    if wait_until 15 "busybox tail -c +$((offset + 1)) $(q "$log") | busybox grep -q 'Loading state'"; then
      record PASS "$label" "Menu + L1 loads the state" "RetroArch read it back"
    else
      record FAIL "$label" "Menu + L1 loads the state" "no 'Loading state' in $log"
    fi
  else
    record SKIP "$label" "save state" "standalone emulator; its saves are not redirected"
  fi

  # Menu + Start: the daemon stops the emulator and the menu must come back.
  dev "$G observe press menu+start" >/dev/null 2>&1
  started=$SECONDS
  if wait_until 25 "[ ! -s /run/ghazal/emulator.toml ]"; then
    record PASS "$label" "Menu + Start quits the emulator" "gone after $((SECONDS - started)) s"
  else
    record FAIL "$label" "Menu + Start quits the emulator" "still running after 25 s; stopping it through the daemon"
    dev "$G ctl stop-emulator" >/dev/null 2>&1 || true
    wait_until 15 "[ ! -s /run/ghazal/emulator.toml ]" || true
  fi
  if [ "$launch" = retroarch ]; then
    if dev "busybox find $directory/states -name '*.state.auto' | busybox grep -q ."; then
      record PASS "$label" "automatic resume state written (scratch)" "the next launch would resume"
    else
      record FAIL "$label" "automatic resume state written (scratch)" "no .state.auto under $directory/states; does the core support states?"
    fi
  fi
  if [ -n "$policy_dir" ]; then
    if wait_until 15 "[ \"\$(cat $policy_dir/scaling_governor)\" = $(plan_get menu_governor) ] && [ \"\$(cat $policy_dir/scaling_min_freq)\" = $(plan_get menu_min_khz) ]"; then
      record PASS "$label" "menu resumes (CPU back to the menu policy)" "$(plan_get menu_governor), floor $(plan_get menu_min_khz) kHz"
    else
      record FAIL "$label" "menu resumes (CPU back to the menu policy)" "$(dev "cat $policy_dir/scaling_governor $policy_dir/scaling_min_freq" | tr '\n' ' ')"
    fi
  fi
  if colors=$(frame_with_content 20 "menu-after-$GAME_NUMBER"); then
    record PASS "$label" "menu is drawn again" "$colors distinct colours"
  else
    record FAIL "$label" "menu is drawn again" "$colors distinct colours"
  fi
  [ -z "$log" ] || evidence "busybox tail -c +$((offset + 1)) $(q "$log")" "game-$GAME_NUMBER.log"
}

# --- main ---------------------------------------------------------------------

trap cleanup EXIT
trap 'exit 130' INT TERM
printf '%-5s %-34s %-42s %s\n' STATE GAME CHECK DETAIL
check_system || { echo "The handheld is unreachable; nothing else was checked."; exit 1; }

if [ -n "$GAMES_FILE" ]; then
  ROMS=()
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in '' | '#'*) ;; *) ROMS+=("$line") ;; esac
  done <"$GAMES_FILE"
  if [ "${#ROMS[@]}" -gt 0 ] && install_wrapper; then
    for rom in "${ROMS[@]}"; do run_game "$rom"; done
  elif [ "${#ROMS[@]}" -gt 0 ]; then
    record FAIL games "bind the scratch wrapper over RetroArch" "mount --bind failed; no game was launched"
  else
    record FAIL games "read the games file" "$GAMES_FILE lists no ROM paths"
  fi
fi

passed=0 skipped=0
for row in "${ROWS[@]}"; do
  case "$row" in PASS\|*) passed=$((passed + 1)) ;; SKIP\|*) skipped=$((skipped + 1)) ;; esac
done
if [ "$FAILURES" -gt 0 ]; then
  echo
  echo "Failed checks:"
  for row in "${ROWS[@]}"; do
    case "$row" in FAIL\|*) IFS='|' read -r _ game check detail <<<"$row"; printf '  %s: %s (%s)\n' "$game" "$check" "$detail" ;; esac
  done
fi
echo
echo "$passed passed, $FAILURES failed, $skipped skipped"
[ "$FAILURES" -eq 0 ]

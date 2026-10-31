# Boot smoke test: starts the QEMU test machine (nix/vm.nix) headless and checks
# that Ghazal as PID 1 gets a stock Linux kernel to a drawn, responsive menu.
#
# This covers the static `ghazal` binary: PID 1, the supervisor, the daemon and
# the menu's KMS and input code, with config/qemu in place of the handheld's
# profile. It does not exercise the H700 kernel, drivers or the Nix runtime.
{ writeShellApplication, socat, coreutils, gnugrep, vm }:
writeShellApplication {
  name = "ghazal-vm-smoke";
  runtimeInputs = [ socat coreutils gnugrep ];
  text = ''
    # Under emulation without KVM the boot takes a minute or two.
    boot_timeout=''${GHAZAL_SMOKE_BOOT_TIMEOUT:-420}
    work=$(mktemp -d)
    failed=0
    qemu_pid=""
    # shellcheck disable=SC2329 # runs from the EXIT trap
    cleanup() {
      if [ -n "$qemu_pid" ]; then kill "$qemu_pid" 2>/dev/null || true; fi
      if [ "$failed" -ne 0 ] && [ -f "$work/serial.log" ]; then
        echo "--- last guest console output ---"
        tail -n 40 "$work/serial.log"
      fi
      if [ -n "''${GHAZAL_SMOKE_KEEP:-}" ]; then echo "kept $work"; else rm -rf "$work"; fi
    }
    trap cleanup EXIT

    pass() { printf 'PASS  %s\n      %s\n' "$1" "$2"; }
    fail() { printf 'FAIL  %s\n      %s\n' "$1" "$2"; failed=1; }

    # Waits for a pattern in the guest console log.
    wait_for() {
      local pattern=$1 seconds=$2 waited=0
      while ! grep -q -- "$pattern" "$work/serial.log"; do
        if ! kill -0 "$qemu_pid" 2>/dev/null; then return 1; fi
        if [ "$waited" -ge "$seconds" ]; then return 1; fi
        sleep 1
        waited=$((waited + 1))
      done
    }

    # Types a command on the guest's serial console and waits for `done` to appear after it.
    guest() { # COMMAND [SECONDS]
      local marker="smoke-done-$RANDOM"
      printf '%s; echo %s\n' "$1" "$marker" >&3
      wait_for "^$marker" "''${2:-30}"
    }

    monitor() { echo "$1" | socat - "UNIX-CONNECT:$work/monitor" >/dev/null; }

    screenshot() {
      rm -f "$1"
      monitor "screendump $1"
      local waited=0
      while [ ! -s "$1" ] && [ "$waited" -lt 20 ]; do sleep 1; waited=$((waited + 1)); done
      sleep 1
      [ -s "$1" ]
    }

    # Distinct pixel colours after the 15-byte "P6\n640 480\n255\n" header.
    colours() { tail -c +16 "$1" | od -An -v -tx1 -w3 | sort -u | wc -l; }

    mkfifo "$work/input"
    exec 3<>"$work/input"
    export GHAZAL_VM_HEADLESS=1 GHAZAL_VM_MONITOR="$work/monitor"
    ${vm}/bin/ghazal-vm <"$work/input" >"$work/serial.log" 2>&1 &
    qemu_pid=$!

    if wait_for '^ghazal# ' "$boot_timeout"; then
      pass "Ghazal boots as PID 1 and starts its console service" "root shell on the guest's serial console"
    else
      fail "Ghazal boots as PID 1 and starts its console service" "no shell prompt within $boot_timeout s"
      exit 1
    fi

    # The console prompt can appear before the menu has drawn: wait for its marker file.
    # shellcheck disable=SC2016
    if guest 'i=0; until [ -s /run/ghazal/first-frame-ready ] || [ $i -ge 120 ]; do sleep 1; i=$((i + 1)); done; cat /run/ghazal/first-frame-ready' 150 \
      && grep -q '^first_frame_kernel_uptime_seconds' "$work/serial.log"; then
      pass "the menu draws its first frame" "$(grep '^first_frame_kernel_uptime_seconds' "$work/serial.log" | head -n 1)"
    else
      fail "the menu draws its first frame" "/run/ghazal/first-frame-ready was never written"
    fi

    # The brackets keep grep from matching its own command line; the guest expands the $().
    # shellcheck disable=SC2016
    if guest 'echo daemons=$(ps | grep -c "[g]hazal daemon") menus=$(ps | grep -c "[g]hazal frontend")' \
      && grep -q '^daemons=[1-9] menus=[1-9]' "$work/serial.log"; then
      pass "the daemon and the menu are both running" "the supervisor started them and they stayed up"
    else
      fail "the daemon and the menu are both running" "ps lacks ghazal daemon or ghazal frontend"
    fi

    if screenshot "$work/menu.ppm" && [ "$(colours "$work/menu.ppm")" -ge 16 ]; then
      pass "the screen shows a menu, not a blank frame" "$(colours "$work/menu.ppm") distinct colours"
    else
      fail "the screen shows a menu, not a blank frame" "fewer than 16 distinct colours on the virtual display"
    fi

    # A (Z in config/qemu) opens Explore Content: a different screen.
    monitor "sendkey z"
    responded=0
    for _ in 1 2 3 4 5 6 7 8; do
      sleep 2
      if screenshot "$work/explore.ppm" && [ "$(cksum <"$work/menu.ppm")" != "$(cksum <"$work/explore.ppm")" ]; then
        responded=1
        break
      fi
    done
    if [ "$responded" -eq 1 ]; then
      pass "the menu responds to the gamepad" "pressing A changed the screen"
    else
      fail "the menu responds to the gamepad" "the screen did not change within 16 s of pressing A"
    fi

    if grep -qi 'panicked\|Kernel panic' "$work/serial.log"; then
      fail "nothing panicked" "$(grep -i 'panicked\|Kernel panic' "$work/serial.log" | head -n 1)"
    else
      pass "nothing panicked" "no panic on the guest console"
    fi
    exit "$failed"
  '';
}

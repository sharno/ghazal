# H700 suspend diagnostics

The normal standalone profile disables suspend until physical wake, display,
input and audio recovery have passed repeated tests. The production kernel
does not expose `pm_test` or a persistent console backend.

Build the separate diagnostic kernel with:

```sh
nix build .#h700-debug-kernel
```

It is the normal kernel recipe (`nix/h700-kernel.nix`) with PM debugging,
dynamic debug, function tracing and ramoops console storage added. Its v2-panel
device tree reserves 1 MiB at `0x7fb00000` for ramoops, before Linux allocates
normal memory or CMA. It does not change the normal image or installed handheld.

The package contains `boot/KERNEL` and `boot/dtb.img`. Like
the normal kernel, every driver is built in, so no modules need to match it.
Keep the normal boot artifacts for recovery. Add `no_console_suspend` and
`initcall_debug` to its diagnostic boot arguments.

Wi-Fi and the panel driver are built in and request firmware before the root
filesystem is mounted. The recipe embeds their firmware, extracted from the
exact checksum-pinned hardware bundle. Runtime firmware files cannot repair an
early probe that already failed.

After a diagnostic boot, first confirm the expected kernel release, panel,
controls and SSH. Inspect `/sys/power/pm_test` and enable
`/sys/power/pm_debug_messages`. Start with `devices`, then `platform` and `core`,
before attempting a real suspend. These tests return after exercising their
selected phases; a failure narrows the device or platform path involved.

To inspect persistent records after Reset:

```sh
mount -t pstore pstore /sys/fs/pstore
ls /sys/fs/pstore
```

Copy records before clearing them. RAM retention through this board's Reset
and successful diagnostic boot still require hardware acceptance. Do not infer
a successful Linux resume from firmware scratch registers without an attempt
identifier; earlier markers may survive from another boot.

The H700 deep-suspend firmware disables the watchdog around WFI, so a
watchdog cannot be assumed to recover every deep-sleep failure. Establish
a recovery route before attempting PM tests.

## Remote kernel trials

An inactive A/B slot protects the installed files, but fallback still requires
a reset. Userspace boot-health checks cannot recover a kernel that hangs before
PID 1. A PMIC watchdog reset in a running OS proves only that reset mechanism:
it does not establish watchdog handover across a normal reboot, firmware and
the next kernel. Verify that full sequence with the known working kernel before
trying a different kernel remotely. Keep physical Reset access available until
that recovery sequence has been demonstrated.

For charging diagnostics, observe USB presence and battery voltage/capacity over
time rather than relying on a single indicator reading. Compare PMIC registers
against the driver cache: the AXP717 input-current register can change when VBUS
is removed, so a cached current-limit readback alone does not establish the
actual hardware limit. Keep register experiments out of production startup
until the resulting charging behavior is verified.

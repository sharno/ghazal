# Boot-test machine: QEMU aarch64 "virt" with a stock nixpkgs kernel and an initramfs
# whose /init is oxmux. virtio-gpu gives the frontend a 640x480 KMS display like the
# handheld's; the virtio keyboard stands in for the gamepad. Only the kernel modules
# the machine needs are included; oxmux's coldplug loads them.
{ lib, runCommand, writeShellApplication, kmod, cpio, xz, qemu, oxmux, kernel, busybox, profile }:

let
  version = kernel.modDirVersion;
  wantedModules = [ "virtio_gpu" "virtio_input" "virtio_pci" "virtio_mmio" "virtio_blk" "evdev" ];

  # The listed modules plus their dependencies, decompressed (oxmux's loader wants plain
  # .ko files) and re-indexed with depmod.
  modules = runCommand "oxmux-vm-modules-${version}" { nativeBuildInputs = [ kmod xz ]; } ''
    src=${kernel.modules}/lib/modules/${version}
    mkdir -p $out/lib/modules/${version}
    for m in ${lib.concatStringsSep " " wantedModules}; do
      modprobe -d ${kernel.modules} -S ${version} --show-depends "$m" 2>/dev/null \
        | awk '$1 == "insmod" { print $2 }'
    done | sort -u | while read -r ko; do
      rel=''${ko#$src/}
      mkdir -p "$out/lib/modules/${version}/$(dirname "$rel")"
      case "$ko" in
        *.xz) xz -dc "$ko" > "$out/lib/modules/${version}/''${rel%.xz}" ;;
        *) cp "$ko" "$out/lib/modules/${version}/$rel" ;;
      esac
    done
    cp $src/modules.builtin* $src/modules.order $out/lib/modules/${version}/ 2>/dev/null || true
    depmod -b $out ${version}
    echo "modules: $(find $out -name '*.ko' | wc -l)"
  '';

  applets = [ "sh" "ls" "cat" "ps" "mount" "umount" "dmesg" "grep" "less" "top" "kill" "ln"
              "mkdir" "rm" "cp" "mv" "echo" "vi" "free" "df" "uname" "sleep" "poweroff" "reboot" ];

  initrd = runCommand "oxmux-vm-initrd" { nativeBuildInputs = [ cpio xz ]; } ''
    root=$PWD/root
    mkdir -p $root/{bin,dev,proc,sys,run,tmp,root,etc,opt/oxmux/etc}
    install -m755 ${oxmux}/bin/oxmux $root/opt/oxmux/oxmux
    ln -s /opt/oxmux/oxmux $root/init
    cp ${profile}/*.toml $root/opt/oxmux/etc/
    install -m755 ${busybox}/bin/busybox $root/bin/busybox
    for a in ${lib.concatStringsSep " " applets}; do ln -s busybox $root/bin/$a; done
    cp -r ${modules}/lib $root/
    chmod -R u+w $root/lib
    # A small fake library so the menu has something to show.
    mkdir -p $root/roms/GBA $root/roms/SNES
    for g in "Pixel Quest" "Orbit Jam" "Neon Drift" "Sky Harbor" "Caverns of Rust"; do touch "$root/roms/GBA/$g.gba"; done
    for g in "Castle Crawl" "Block Party"; do touch "$root/roms/SNES/$g.sfc"; done
    (cd $root && find . | sort | cpio -o -H newc -R 0:0 --quiet) | xz --check=crc32 -T0 > $out
  '';

  runner = writeShellApplication {
    name = "oxmux-vm";
    runtimeInputs = [ qemu ];
    text = ''
      # Usage: oxmux-vm [extra qemu args]. Serial console (root shell) on stdio.
      # OXMUX_VM_HEADLESS=1: no window; QEMU monitor on $OXMUX_VM_MONITOR (a unix socket)
      # for scripted key presses and screendumps.
      display=(-display "gtk,zoom-to-fit=on")
      if [ -n "''${OXMUX_VM_HEADLESS:-}" ]; then
        display=(-display none -monitor "unix:''${OXMUX_VM_MONITOR:-/tmp/oxmux-vm.monitor},server,nowait")
      fi
      exec qemu-system-aarch64 \
        -machine virt -cpu cortex-a53 -smp 4 -m 1024 \
        -kernel ${kernel}/Image -initrd ${initrd} \
        -append "rdinit=/init console=ttyAMA0 loglevel=4" \
        -device virtio-gpu-pci,xres=640,yres=480 \
        -device virtio-keyboard-pci \
        -serial mon:stdio "''${display[@]}" "$@"
    '';
  };
in
{ inherit initrd runner modules; }

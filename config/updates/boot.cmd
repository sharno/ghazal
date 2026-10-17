# Stored in U-Boot's compiled default environment, never in an update slot.
mmc dev 0
if test -z "${BOOT_ORDER}"; then
    setenv BOOT_ORDER "A B"
    setenv BOOT_A_LEFT 3
    setenv BOOT_B_LEFT 3
fi
for slot in ${BOOT_ORDER}; do
    if test "${slot}" = "A"; then
        setenv rootpart 2
        setenv left ${BOOT_A_LEFT}
    else
        setenv rootpart 5
        setenv left ${BOOT_B_LEFT}
    fi
    if test ${left} -gt 0; then
        setexpr left ${left} - 1
        if test "${slot}" = "A"; then setenv BOOT_A_LEFT ${left}; else setenv BOOT_B_LEFT ${left}; fi
        if saveenv; then
            setenv bootargs "root=/dev/mmcblk0p${rootpart} rootfstype=ext4 rootwait ro init=/sbin/init rauc.slot=${slot} console=ttyS0,115200 console=tty0 quiet panic=10 rtw88_core.disable_lps_deep=Y"
            if load mmc 0:${rootpart} ${kernel_addr_r} /boot/KERNEL; then
                if load mmc 0:${rootpart} ${fdt_addr_r} /boot/dtb.img; then
                    booti ${kernel_addr_r} - ${fdt_addr_r}
                fi
            fi
        fi
    fi
done
# No usable slot: leave the card intact for recovery/reflash.
echo "Ghazal: no bootable OS slot"
reset

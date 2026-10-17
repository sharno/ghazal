//! Executes the production slot-selection script with observable disk/boot commands.
//! Bash models command outcomes; this does not validate U-Boot drivers or H700 hardware.
use crate::Result;
use std::{fs, path::Path, process::Command};
pub fn exercise(directory: &Path) -> Result<()> {
    let directory = directory.join("boot");
    fs::create_dir(&directory)?;
    let script = directory.join("boot.sh");
    fs::write(
        &script,
        format!(
            "{}\n{}",
            r#"
set -u
if test -f env; then . ./env; else BOOT_ORDER='A B'; BOOT_A_LEFT=3; BOOT_B_LEFT=3; fi
kernel_addr_r=0x40080000; fdt_addr_r=0x4fa00000
setenv() { local name="$1"; shift; printf -v "$name" '%s' "$*"; }
setexpr() { local name="$1"; local value="$2"; printf -v "$name" '%s' "$((value - 1))"; }
mmc() { return 0; }
saveenv() { printf 'BOOT_ORDER=%q\nBOOT_A_LEFT=%q\nBOOT_B_LEFT=%q\n' "$BOOT_ORDER" "$BOOT_A_LEFT" "$BOOT_B_LEFT" > env; }
load() { return 0; }
booti() { printf '%s' "$slot" > selected; exit 0; }
reset() { exit 44; }
"#,
            include_str!("../../../config/updates/boot.cmd")
        ),
    )?;
    let boot = |expected: &str| -> Result<()> {
        if !Command::new("bash")
            .arg(&script)
            .current_dir(&directory)
            .status()?
            .success()
        {
            return Err("boot selection failed".into());
        }
        if fs::read_to_string(directory.join("selected"))? != expected {
            return Err(format!("expected slot {expected}").into());
        }
        Ok(())
    };
    // RAUC activates B; three unconfirmed boots must exhaust B, then boot A.
    fs::write(
        directory.join("env"),
        "BOOT_ORDER='B A'\nBOOT_A_LEFT=3\nBOOT_B_LEFT=3\n",
    )?;
    for slot in ["B", "B", "B", "A"] {
        boot(slot)?;
    }
    // Confirmed B is retained through repeated boots, with health marking good.
    for _ in 0..5 {
        fs::write(
            directory.join("env"),
            "BOOT_ORDER='B A'\nBOOT_A_LEFT=3\nBOOT_B_LEFT=3\n",
        )?;
        boot("B")?;
    }
    // Exhausted slots must not silently erase the attempt history and retry forever.
    fs::write(
        directory.join("env"),
        "BOOT_ORDER='B A'\nBOOT_A_LEFT=0\nBOOT_B_LEFT=0\n",
    )?;
    let result = Command::new("bash")
        .arg(&script)
        .current_dir(&directory)
        .status()?;
    if result.code() != Some(44) {
        return Err("exhausted slots did not enter recovery".into());
    }
    println!("Production boot script: three failed trials fall back; confirmed slot persists; exhausted slots enter recovery.");
    Ok(())
}

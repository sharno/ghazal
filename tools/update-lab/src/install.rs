//! Optional root-only lab: actual RAUC service, private D-Bus, disposable file slots.
//! Never opens a physical disk. Requires loop, squashfs and dm-verity on the host.
use crate::Result;
use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
struct Processes(Vec<Child>);
impl Drop for Processes {
    fn drop(&mut self) {
        for child in self.0.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
pub fn exercise(directory: &Path, expected: &[u8]) -> Result<()> {
    let bus = format!("unix:path={}/bus", directory.display());
    let mut processes = Processes(vec![Command::new("dbus-daemon")
        .args([
            "--session",
            "--nofork",
            "--print-address=1",
            "--address",
            &bus,
        ])
        .stdout(Stdio::piped())
        .spawn()?]);
    let mut address = String::new();
    BufReader::new(processes.0[0].stdout.take().ok_or("missing bus stdout")?)
        .read_line(&mut address)?;
    if !address.starts_with(&bus) {
        return Err("private D-Bus did not report readiness".into());
    }
    fs::write("slot-a", vec![0x11; expected.len()])?;
    fs::write("slot-b", vec![0x22; expected.len()])?;
    fs::write("primary", "A")?;
    fs::write("chooser", format!("#!/bin/sh\ncase \"$1\" in\nget-primary) cat '{0}/primary';;\nset-primary) printf '%s' \"$2\" > '{0}/primary';;\nget-state) echo good;;\nset-state) exit 0;;\n*) exit 1;;\nesac\n", directory.display()))?;
    fs::set_permissions("chooser", fs::Permissions::from_mode(0o700))?;
    fs::write("system.conf", format!("[system]\ncompatible=ghazal-lab-v1\nbootloader=custom\nmountprefix={0}/mount\ndata-directory={0}/state\n[keyring]\npath={0}/release.pem\n[handlers]\nbootloader-custom-backend={0}/chooser\n[slot.rootfs.0]\ndevice={0}/slot-a\ntype=raw\nbootname=A\n[slot.rootfs.1]\ndevice={0}/slot-b\ntype=raw\nbootname=B\n", directory.display()))?;
    let command = || {
        let mut c = Command::new("rauc");
        c.env("DBUS_SYSTEM_BUS_ADDRESS", &bus)
            .args(["--conf", "system.conf"]);
        c
    };
    processes.0.push(
        command()
            .args(["--override-boot-slot=A", "service"])
            .spawn()?,
    );
    let mut ready = false;
    for _ in 0..50 {
        if command().arg("status").output()?.status.success() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !ready {
        return Err("private RAUC service did not become ready".into());
    }
    for rejected in [
        "untrusted.raucb",
        "incompatible.raucb",
        "payload-corrupt.raucb",
    ] {
        if command().args(["install", rejected]).status()?.success() {
            return Err(format!("accepted invalid bundle {rejected}").into());
        }
        if fs::read("slot-a")? != vec![0x11; expected.len()]
            || fs::read_to_string("primary")? != "A"
        {
            return Err("failed install changed active slot or boot selection".into());
        }
    }
    // FAT/exFAT cannot enforce per-file Unix permissions. Verity must remain
    // safe and installable without requiring an exclusive writable-file lease.
    fs::set_permissions("update.raucb", fs::Permissions::from_mode(0o666))?;
    if !command()
        .args(["install", "update.raucb"])
        .status()?
        .success()
    {
        return Err("real RAUC installation failed".into());
    }
    if fs::read("slot-a")? != vec![0x11; expected.len()] {
        return Err("active slot was modified".into());
    }
    if fs::read("slot-b")? != expected {
        return Err("inactive slot does not match signed payload".into());
    }
    if fs::read_to_string("primary")? != "B" {
        return Err("installed slot was not activated".into());
    }
    drop(processes);
    println!(
        "Real RAUC installation preserved active A, wrote signed payload to B and activated B."
    );
    Ok(())
}

//! Actual U-Boot userspace tools against redundant environments in a regular file.
use crate::Result;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
    process::Command,
};
pub fn exercise(directory: &Path) -> Result<()> {
    fs::write("env.txt", "BOOT_ORDER=A B\nBOOT_A_LEFT=3\nBOOT_B_LEFT=3\n")?;
    crate::run(
        "mkenvimage",
        &["-r", "-s", "0x20000", "-o", "env.bin", "env.txt"],
    )?;
    let path = directory.join("environment-disk");
    let mut disk = fs::File::create(&path)?;
    disk.set_len(16 * 1024 * 1024)?;
    let seed = fs::read("env.bin")?;
    for offset in [0x400000, 0x420000] {
        disk.seek(SeekFrom::Start(offset))?;
        disk.write_all(&seed)?;
    }
    disk.sync_all()?;
    fs::write(
        "fw_env.config",
        format!(
            "{0} 0x400000 0x20000\n{0} 0x420000 0x20000\n",
            path.display()
        ),
    )?;
    crate::run("fw_setenv", &["-c", "fw_env.config", "BOOT_ORDER", "B A"])?;
    let read = || -> Result<String> {
        let result = Command::new("fw_printenv")
            .args(["-c", "fw_env.config", "-n", "BOOT_ORDER"])
            .output()?;
        if !result.status.success() {
            return Err("failed to read redundant environment".into());
        }
        Ok(String::from_utf8(result.stdout)?.trim().into())
    };
    if read()? != "B A" {
        return Err("boot selection was not persisted".into());
    }
    // With either copy destroyed, the other must still give a complete environment.
    let complete = fs::read(&path)?;
    for offset in [0x400000, 0x420000] {
        disk.seek(SeekFrom::Start(offset))?;
        disk.write_all(&vec![0; 0x20000])?;
        disk.sync_all()?;
        if !matches!(read()?.as_str(), "A B" | "B A") {
            return Err("partial environment accepted".into());
        }
        fs::write(&path, &complete)?;
    }
    println!("Real U-Boot environment tools preserve a readable boot selection with either redundant copy corrupted.");
    Ok(())
}

//! Exercise the real RAUC verifier with disposable keys and bundles. Never opens a disk.
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Command,
};
mod boot;
mod environment;
mod install;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn run(program: &str, args: &[&str]) -> Result<()> {
    if !Command::new(program).args(args).status()?.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(())
}
fn verify(bundle: &str, cert: &str) -> Result<bool> {
    Ok(Command::new("rauc")
        .args(["info", "--keyring", cert, bundle])
        .output()?
        .status
        .success())
}
fn main() -> Result<()> {
    let directory = std::env::temp_dir().join(format!("ghazal-update-lab-{}", std::process::id()));
    fs::create_dir(&directory)?;
    boot::exercise(&directory)?;
    std::env::set_current_dir(&directory)?;
    environment::exercise(&directory)?;
    for name in ["release", "untrusted"] {
        run(
            "openssl",
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                &format!("{name}.key"),
                "-out",
                &format!("{name}.pem"),
                "-days",
                "1",
                "-subj",
                "/CN=Ghazal update lab/",
            ],
        )?;
    }
    fs::create_dir("bundle")?;
    let mut payload = vec![0; 1024 * 1024];
    fs::File::open("/dev/urandom")?.read_exact(&mut payload)?;
    fs::write("bundle/root.ext4", &payload)?;
    fs::write("bundle/manifest.raucm", "[update]\ncompatible=ghazal-lab-v1\nversion=lab\n[bundle]\nformat=verity\n[image.rootfs]\nfilename=root.ext4\n")?;
    run(
        "rauc",
        &[
            "bundle",
            "--cert",
            "release.pem",
            "--key",
            "release.key",
            "bundle",
            "update.raucb",
        ],
    )?;
    if !verify("update.raucb", "release.pem")? {
        return Err("valid signed bundle rejected".into());
    }
    if verify("update.raucb", "untrusted.pem")? {
        return Err("untrusted signature accepted".into());
    }
    fs::copy("update.raucb", "truncated.raucb")?;
    fs::OpenOptions::new()
        .write(true)
        .open("truncated.raucb")?
        .set_len(64)?;
    if verify("truncated.raucb", "release.pem")? {
        return Err("truncated bundle accepted".into());
    }
    // Destroy the CMS signature at the end, not merely unverified payload blocks.
    fs::copy("update.raucb", "corrupt.raucb")?;
    let mut corrupt = fs::OpenOptions::new().write(true).open("corrupt.raucb")?;
    corrupt.seek(SeekFrom::End(-32))?;
    corrupt.write_all(&[0; 16])?;
    if verify("corrupt.raucb", "release.pem")? {
        return Err("corrupt signature accepted".into());
    }
    if std::env::args().any(|arg| arg == "--install") {
        run(
            "rauc",
            &[
                "bundle",
                "--cert",
                "untrusted.pem",
                "--key",
                "untrusted.key",
                "bundle",
                "untrusted.raucb",
            ],
        )?;
        let manifest = fs::read_to_string("bundle/manifest.raucm")?;
        fs::write(
            "bundle/manifest.raucm",
            manifest.replace("ghazal-lab-v1", "wrong-device"),
        )?;
        run(
            "rauc",
            &[
                "bundle",
                "--cert",
                "release.pem",
                "--key",
                "release.key",
                "bundle",
                "incompatible.raucb",
            ],
        )?;
        fs::copy("update.raucb", "payload-corrupt.raucb")?;
        let mut corrupt = fs::OpenOptions::new()
            .write(true)
            .open("payload-corrupt.raucb")?;
        corrupt.seek(SeekFrom::Start(128 * 1024))?;
        corrupt.write_all(&[0; 4096])?;
        install::exercise(&directory, &payload)?;
    }
    std::env::set_current_dir(Path::new("/"))?;
    fs::remove_dir_all(directory)?;
    println!(
        "RAUC accepted the trusted bundle and rejected wrong-key, truncated and corrupted bundles."
    );
    Ok(())
}

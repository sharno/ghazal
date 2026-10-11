//! Convert a complete MUOS data backup on a mounted SD2 into oxmux's layout.
//! This command never formats disks or changes the original backup.
use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct RestoredCard {
    root: PathBuf,
    syncthing_config: String,
}
impl RestoredCard {
    fn validate(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let mounts = fs::read_to_string("/proc/mounts")?;
        let mounted = mounts.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() >= 3 && fields[0].starts_with("/dev/")
                && fields[1] == root.to_string_lossy() && fields[2] == "ext4"
        });
        if !mounted { return Err("destination must be a mounted ext4 block device".into()); }
        for name in ["ROMS", "MUOS/bios", "MUOS/save", "MUOS/info/catalogue", "MUOS/syncthing"] {
            if !root.join(name).is_dir() { return Err(format!("restoration incomplete: {name}").into()); }
        }
        for name in ["BIOS", "SAVES", ".oxmux/syncthing"] {
            if root.join(name).exists() { return Err(format!("destination already exists: {name}").into()); }
        }
        for name in ["MUOS/syncthing/cert.pem", "MUOS/syncthing/key.pem", "MUOS/save/.stfolder", "MUOS/info/catalogue/.stfolder"] {
            if !root.join(name).exists() { return Err(format!("missing identity or sync marker: {name}").into()); }
        }
        let mut config = fs::read_to_string(root.join("MUOS/syncthing/config.xml"))?;
        for (old, new) in [
            ("path=\"/run/muos/storage/save\"", "path=\"/mnt/games/SAVES\""),
            ("path=\"/run/muos/storage/info/catalogue\"", "path=\"/mnt/games/MUOS/info/catalogue\""),
        ] {
            if config.matches(old).count() != 1 { return Err(format!("unexpected sync configuration: {old}").into()); }
            config = config.replace(old, new);
        }
        config = config.replace("label=\"/run/muos/storage/save/\"", "label=\"Handheld saves\"")
            .replace("label=\"/run/muos/storage/info/catalogue\"", "label=\"Handheld artwork\"");
        Ok(Self { root, syncthing_config: config })
    }

    fn convert(self) -> Result<()> {
        fs::create_dir_all(self.root.join(".oxmux"))?;
        let config = self.root.join("MUOS/syncthing/config.xml");
        fs::copy(&config, config.with_extension("xml.before-oxmux"))?;
        let staged = config.with_extension("xml.new");
        fs::write(&staged, self.syncthing_config)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))?;
        fs::rename(staged, config)?;
        for (source, destination) in [("MUOS/bios", "BIOS"), ("MUOS/save", "SAVES"), ("MUOS/syncthing", ".oxmux/syncthing")] {
            fs::rename(self.root.join(source), self.root.join(destination))?;
        }
        for name in [".oxmux", ".oxmux/syncthing"] {
            fs::set_permissions(self.root.join(name), fs::Permissions::from_mode(0o700))?;
        }
        for name in ["cert.pem", "key.pem", "https-cert.pem", "https-key.pem", "api.txt", "config.xml", "config.xml.before-oxmux"] {
            fs::set_permissions(self.root.join(".oxmux/syncthing").join(name), fs::Permissions::from_mode(0o600))?;
        }
        println!("Converted SD2: ROMS, BIOS, SAVES and persistent Syncthing; remaining MUOS data preserved.");
        Ok(())
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = args.next().ok_or("usage: oxmux-data-migration MOUNTPOINT")?;
    if args.next().is_some() { return Err("expected exactly one mountpoint".into()); }
    RestoredCard::validate(Path::new(&root))?.convert()
}

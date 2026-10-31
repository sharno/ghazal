//! Static checks over the built runtime tree against the shipped profile.
//!
//! Everything is derived from the profile (cores from `frontend.toml`, commands
//! from `system.toml`, the pad from `gamepad.cfg`), so features that add a core,
//! service or standalone emulator are covered without touching this file.

use std::fmt::Display;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::profile::Profile;
use crate::sdl;
use crate::tree::{read_elf, Elf, ElfKind, Tree};

type Problems = Vec<String>;
type Gate = fn(&Ctx) -> Result<String, Problems>;

struct Ctx<'a> {
    tree: &'a Tree,
    profile: &'a Profile,
}

pub struct Check {
    pub name: &'static str,
    pub outcome: Result<String, Problems>,
}

/// One interpreter over the table: run every gate, keep every result.
pub fn run(tree: &Tree, profile: &Profile) -> Vec<Check> {
    let ctx = Ctx { tree, profile };
    GATES.iter().map(|(name, gate)| Check { name, outcome: gate(&ctx) }).collect()
}

const GATES: &[(&str, Gate)] = &[
    ("gamepad profile is where RetroArch looks", gamepad_profile),
    ("RetroArch's directories exist", retroarch_dirs),
    ("RetroArch is an aarch64 executable", retroarch_binary),
    ("every core named in frontend.toml is present", cores),
    ("standalone emulators are present", standalone_commands),
    ("PPSSPP's assets are present and its path is baked in", ppsspp_assets),
    ("SDL controller mapping matches the pad", sdl_mapping),
    ("boot plan commands from the runtime exist", boot_commands),
    ("runtime /etc files exist", etc_files),
];

fn single<T, E: Display>(result: Result<T, E>) -> Result<T, Problems> {
    result.map_err(|error| vec![error.to_string()])
}

fn problems_or<T>(problems: Problems, ok: impl FnOnce() -> T) -> Result<T, Problems> {
    if problems.is_empty() { Ok(ok()) } else { Err(problems) }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// RetroArch's joypad driver reads profiles only from the folder named after
/// it (`autoconfig/udev/`), never from the root of the autoconfig directory:
/// betas 8 and 9 shipped the pad's profile one level up, so no game had controls.
fn gamepad_profile(ctx: &Ctx) -> Result<String, Problems> {
    let cfg = &ctx.profile.retroarch_cfg;
    let dir = single(cfg.require("joypad_autoconfig_dir", "retroarch.cfg"))?;
    let driver = single(cfg.require("input_joypad_driver", "retroarch.cfg"))?;
    let pad = &ctx.profile.pad;
    let wanted = Path::new(dir).join(driver).join(format!("{}.cfg", pad.name));
    let found = ctx.tree.find(&wanted).map_err(|error| vec![format!("{error}; RetroArch searches only {dir}/{driver}/, so games would start without the pad's buttons")])?;
    let installed = single(fs::read_to_string(&found))?;
    let mut problems = Problems::new();
    if installed != ctx.profile.gamepad_cfg_text {
        problems.push(format!("{} differs from the profile's gamepad.cfg", wanted.display()));
    }
    if ctx.profile.device_gamepad != pad.name {
        problems.push(format!("device.toml names the pad {:?} but gamepad.cfg matches {:?}", ctx.profile.device_gamepad, pad.name));
    }
    problems_or(problems, || format!("{} is identical to gamepad.cfg", wanted.display()))
}

fn retroarch_dirs(ctx: &Ctx) -> Result<String, Problems> {
    let cfg = &ctx.profile.retroarch_cfg;
    let mut dirs: Vec<PathBuf> = ["libretro_directory", "joypad_autoconfig_dir"]
        .iter()
        .filter_map(|key| cfg.get(key))
        .map(PathBuf::from)
        .collect();
    dirs.extend(ctx.profile.retroarch.core_dirs.iter().take(1).cloned());
    let problems: Problems = dirs
        .iter()
        .filter_map(|dir| ctx.tree.find(dir).and_then(|found| if found.is_dir() { Ok(()) } else { Err(format!("{} is not a directory", dir.display())) }).err())
        .collect();
    problems_or(problems, || format!("{} directories checked", dirs.len()))
}

fn expect_elf(path: &Path, kinds: &[ElfKind]) -> Result<Elf, String> {
    let elf = read_elf(path)?;
    if !elf.aarch64 {
        return Err(format!("{} is not built for aarch64", path.display()));
    }
    if !kinds.contains(&elf.kind) {
        return Err(format!("{} is {:?}, expected one of {:?}", path.display(), elf.kind, kinds));
    }
    Ok(elf)
}

fn retroarch_binary(ctx: &Ctx) -> Result<String, Problems> {
    let bin = &ctx.profile.retroarch.bin;
    let file = single(ctx.tree.is_executable(bin))?;
    single(expect_elf(&file, &[ElfKind::Executable, ElfKind::Shared]))?;
    Ok(format!("{} -> {}", bin.display(), file.display()))
}

/// The libretro entry point every core exports; a truncated or wrong-arch
/// build lacks it.
const LIBRETRO_ENTRY: &[u8] = b"retro_run";

fn cores(ctx: &Ctx) -> Result<String, Problems> {
    let cores = ctx.profile.cores();
    let problems: Problems = cores
        .iter()
        .filter_map(|core| {
            let found = ctx.profile.retroarch.core_dirs.iter().find_map(|dir| ctx.tree.find(&dir.join(core)).ok());
            let Some(file) = found else {
                return Some(format!("{core} is in none of {:?}", ctx.profile.retroarch.core_dirs));
            };
            let loadable = expect_elf(&file, &[ElfKind::Shared]).and_then(|_| {
                let bytes = fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
                if contains(&bytes, LIBRETRO_ENTRY) { Ok(()) } else { Err(format!("{core} does not export the libretro API")) }
            });
            loadable.err()
        })
        .collect();
    problems_or(problems, || format!("{} cores", cores.len()))
}

/// A standalone emulator may be a wrapper script; anything else must be an
/// aarch64 executable.
fn expect_program(path: &Path) -> Result<(), String> {
    let mut start = [0u8; 2];
    let script = fs::File::open(path).and_then(|mut file| file.read_exact(&mut start)).is_ok() && start == *b"#!";
    if script { Ok(()) } else { expect_elf(path, &[ElfKind::Executable, ElfKind::Shared]).map(drop) }
}

fn standalone_commands(ctx: &Ctx) -> Result<String, Problems> {
    let commands: Vec<_> = ctx.profile.standalone().collect();
    let problems: Problems = commands
        .iter()
        .filter_map(|(system, argv, _)| {
            ctx.tree
                .is_executable(Path::new(&argv[0]))
                .and_then(|file| expect_program(&file))
                .map_err(|error| format!("{system}: {error}"))
                .err()
        })
        .collect();
    problems_or(problems, || format!("{} commands", commands.len()))
}

/// Files PPSSPP cannot start without. It reads them from the directory baked
/// into its binary at build time (nix/runtime/ppsspp.nix).
const PPSSPP_FILES: [&str; 5] = ["font_atlas.zim", "ppge_atlas.zim", "langregion.ini", "compat.ini", "gamecontrollerdb.txt"];
const PPSSPP_DIRS: [&str; 3] = ["flash0/font", "lang", "shaders"];

fn is_ppsspp(argv: &[String]) -> bool {
    Path::new(&argv[0]).file_name().is_some_and(|name| name.to_string_lossy().contains("ppsspp"))
}

fn ppsspp_assets(ctx: &Ctx) -> Result<String, Problems> {
    let emulators: Vec<_> = ctx.profile.standalone().filter(|(_, argv, _)| is_ppsspp(argv)).collect();
    if emulators.is_empty() {
        return Ok("no standalone PPSSPP in the profile".into());
    }
    let mut problems = Problems::new();
    for (system, argv, _) in &emulators {
        let binary = match ctx.tree.find(Path::new(&argv[0])) {
            Ok(binary) => binary,
            Err(error) => {
                problems.push(format!("{system}: {error}"));
                continue;
            }
        };
        let Some(prefix) = binary.parent().and_then(Path::parent) else {
            problems.push(format!("{system}: {} has no package directory", binary.display()));
            continue;
        };
        let assets = prefix.join("share/ppsspp");
        problems.extend(PPSSPP_FILES.iter().map(|file| assets.join("assets").join(file)).filter(|file| !file.is_file()).map(|file| format!("{system}: missing {}", file.display())));
        problems.extend(PPSSPP_DIRS.iter().map(|dir| assets.join("assets").join(dir)).filter(|dir| !dir.is_dir()).map(|dir| format!("{system}: missing directory {}", dir.display())));
        // The build rewrites PPSSPP's asset path to its own store path.
        match fs::read(&binary) {
            Ok(bytes) if contains(&bytes, assets.to_string_lossy().as_bytes()) => {}
            Ok(_) => problems.push(format!("{system}: {} does not contain {}, so it would look for its assets elsewhere", binary.display(), assets.display())),
            Err(error) => problems.push(format!("{system}: {}: {error}", binary.display())),
        }
    }
    problems_or(problems, || format!("{} PPSSPP command(s)", emulators.len()))
}

/// SDL gives standalone emulators the pad through `SDL_GAMECONTROLLERCONFIG`;
/// without a mapping that matches this pad's GUID the buttons do nothing.
fn sdl_mapping(ctx: &Ctx) -> Result<String, Problems> {
    let profile = ctx.profile;
    let emulators: Vec<_> = profile.standalone().filter(|(_, argv, _)| is_ppsspp(argv)).collect();
    if emulators.is_empty() {
        return Ok("no standalone SDL emulator in the profile".into());
    }
    let mut problems = Problems::new();
    for (system, _, env) in &emulators {
        let Some(text) = env.get("SDL_GAMECONTROLLERCONFIG") else {
            problems.push(format!("{system}: SDL_GAMECONTROLLERCONFIG is not set, so SDL sees no controller mapping"));
            continue;
        };
        let mapping = match sdl::Mapping::parse(text) {
            Ok(mapping) => mapping,
            Err(error) => {
                problems.push(format!("{system}: {error}"));
                continue;
            }
        };
        if mapping.guid != sdl::guid(&profile.pad) {
            problems.push(format!("{system}: mapping GUID {} is not the pad's {}", mapping.guid, sdl::guid(&profile.pad)));
        }
        if mapping.name != profile.pad.name {
            problems.push(format!("{system}: mapping is named {:?}, the pad {:?}", mapping.name, profile.pad.name));
        }
        for (control, binding) in sdl::BUTTONS {
            let retroarch = profile.gamepad_cfg.get(&format!("input_{binding}_btn")).and_then(|index| index.parse::<u32>().ok());
            if mapping.button(control).is_none() || mapping.button(control) != retroarch {
                problems.push(format!("{system}: SDL {control} is {:?} but RetroArch's {binding} is {retroarch:?}", mapping.fields.get(control)));
            }
        }
        for (control, binding) in sdl::AXES {
            let retroarch = profile.gamepad_cfg.get(&format!("input_{binding}_axis")).and_then(|axis| axis.trim_start_matches('+').parse::<u32>().ok());
            if mapping.axis(control).is_none() || mapping.axis(control) != retroarch {
                problems.push(format!("{system}: SDL {control} is {:?} but RetroArch's {binding} is {retroarch:?}", mapping.fields.get(control)));
            }
        }
    }
    problems_or(problems, || format!("GUID {} and {} buttons agree with gamepad.cfg", sdl::guid(&profile.pad), sdl::BUTTONS.len()))
}

/// Commands the boot plan starts from `/usr`: the runtime provides all of
/// them. Ghazal itself (`/opt/ghazal`) and BusyBox are added by the image
/// builder; call a BusyBox applet as `busybox <applet>` rather than through a
/// link the image may not have.
fn boot_commands(ctx: &Ctx) -> Result<String, Problems> {
    let mut wanted: Vec<&str> = ctx.profile.commands.iter().map(|argv| argv[0].as_str()).filter(|program| program.starts_with("/usr/")).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let problems: Problems = wanted
        .iter()
        .filter_map(|program| ctx.tree.is_executable(Path::new(program)).err().map(|error| format!("{error}; the boot plan runs it (BusyBox applets are called as `busybox <applet>`)")))
        .collect();
    problems_or(problems, || format!("{} commands", wanted.len()))
}

/// Files the image builder does not create: TLS roots for the updater and
/// thumbnail downloads, and the D-Bus policy iwd needs to own its name.
const ETC_FILES: [&str; 2] = ["etc/ssl/certs/ca-certificates.crt", "etc/dbus-1/system.d/iwd.conf"];

fn etc_files(ctx: &Ctx) -> Result<String, Problems> {
    let problems: Problems = ETC_FILES
        .iter()
        .filter_map(|file| {
            ctx.tree
                .find(&Path::new("/").join(file))
                .and_then(|found| if fs::metadata(&found).is_ok_and(|meta| meta.is_file() && meta.len() > 0) { Ok(()) } else { Err(format!("{file} is empty or not a file")) })
                .err()
        })
        .collect();
    problems_or(problems, || format!("{} files", ETC_FILES.len()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;
    use crate::tree::testing::{elf, TempDir};

    /// The shipped profile with a runtime tree generated from what it demands;
    /// each test then breaks one thing. A profile change that adds a core,
    /// service or emulator keeps the complete tree complete.
    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new("layout");
            let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/rg40xxv-rocknix");
            for file in ["device.toml", "frontend.toml", "system.toml", "gamepad.cfg", "retroarch.cfg"] {
                dir.write(&format!("profile/{file}"), &fs::read(shipped.join(file)).unwrap());
            }
            let fixture = Self { dir };
            fixture.build_tree(&fixture.profile());
            fixture
        }

        fn profile(&self) -> Profile {
            Profile::load(&self.dir.path().join("profile")).unwrap()
        }

        fn tree_path(&self, device_path: &str) -> PathBuf {
            self.dir.path().join("tree").join(device_path.trim_start_matches('/'))
        }

        /// An executable in a store directory, linked from `device_path` like the runtime tree does.
        fn program(&self, device_path: &Path, contents: &[u8]) -> PathBuf {
            let name = device_path.file_name().unwrap().to_string_lossy().into_owned();
            let store = self.dir.path().join(format!("store/{name}/bin/{name}"));
            fs::create_dir_all(store.parent().unwrap()).unwrap();
            fs::write(&store, contents).unwrap();
            fs::set_permissions(&store, fs::Permissions::from_mode(0o755)).unwrap();
            let link = self.tree_path(&device_path.to_string_lossy());
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&link);
            symlink(&store, link).unwrap();
            store
        }

        fn build_tree(&self, profile: &Profile) {
            self.program(&profile.retroarch.bin, &elf(3, 183));
            let mut core = elf(3, 183);
            core.extend_from_slice(LIBRETRO_ENTRY);
            for name in profile.cores() {
                self.program(&profile.retroarch.core_dirs[0].join(name), &core);
            }
            for (_, argv, _) in profile.standalone() {
                let mut binary = elf(3, 183);
                if is_ppsspp(argv) {
                    let package = self.dir.path().join("store/ppsspp");
                    binary.extend_from_slice(package.join("share/ppsspp").to_string_lossy().as_bytes());
                    for file in PPSSPP_FILES {
                        self.dir.write(&format!("store/ppsspp/share/ppsspp/assets/{file}"), b"x");
                    }
                    for dir in PPSSPP_DIRS {
                        self.dir.write(&format!("store/ppsspp/share/ppsspp/assets/{dir}/x"), b"x");
                    }
                }
                self.program(Path::new(&argv[0]), &binary);
            }
            for argv in profile.commands.iter().filter(|argv| argv[0].starts_with("/usr/")) {
                self.program(Path::new(&argv[0]), &elf(2, 183));
            }
            let cfg = &profile.retroarch_cfg;
            let autoconfig = Path::new(cfg.get("joypad_autoconfig_dir").unwrap()).join(cfg.get("input_joypad_driver").unwrap());
            let profile_file = autoconfig.join(format!("{}.cfg", profile.pad.name));
            let installed = self.tree_path(&profile_file.to_string_lossy());
            fs::create_dir_all(installed.parent().unwrap()).unwrap();
            fs::write(installed, &profile.gamepad_cfg_text).unwrap();
            for file in ETC_FILES {
                self.dir.write(&format!("tree/{file}"), b"x");
            }
        }

        /// Rewrites a profile file, insisting the text exists so a renamed key cannot hide a test.
        fn edit_profile(&self, file: &str, from: &str, to: &str) {
            let path = self.dir.path().join("profile").join(file);
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(from), "{file} no longer contains {from:?}");
            fs::write(path, text.replace(from, to)).unwrap();
        }

        fn failures(&self) -> Vec<String> {
            run(&Tree::new(&self.dir.path().join("tree")), &self.profile()).into_iter().filter_map(|check| check.outcome.err()).flatten().collect()
        }
    }

    #[test]
    fn a_complete_tree_passes_every_gate() {
        let failures = Fixture::new().failures();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The regression that shipped in betas 8 and 9.
    #[test]
    fn a_gamepad_profile_outside_the_driver_folder_fails() {
        let fixture = Fixture::new();
        let autoconfig = fixture.tree_path("/usr/share/libretro/autoconfig");
        fs::rename(autoconfig.join("udev/H700 Gamepad.cfg"), autoconfig.join("H700 Gamepad.cfg")).unwrap();
        let failures = fixture.failures();
        assert!(failures.iter().any(|failure| failure.contains("autoconfig/udev/")), "{failures:#?}");
    }

    #[test]
    fn a_stale_gamepad_profile_fails() {
        let fixture = Fixture::new();
        fs::write(fixture.tree_path("/usr/share/libretro/autoconfig/udev/H700 Gamepad.cfg"), "input_device = \"H700 Gamepad\"\n").unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("differs")));
    }

    #[test]
    fn a_missing_or_foreign_core_fails() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let core = profile.cores()[0];
        let link = fixture.tree_path(&format!("/usr/lib/libretro/{core}"));
        let store = fs::read_link(&link).unwrap();
        fs::remove_file(&link).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains(&format!("{core} is in none of"))));

        let mut x86 = elf(3, 62);
        x86.extend_from_slice(LIBRETRO_ENTRY);
        fs::write(&store, &x86).unwrap();
        symlink(&store, &link).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("not built for aarch64")));

        fs::write(&store, elf(3, 183)).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("does not export the libretro API")));
    }

    #[test]
    fn missing_ppsspp_assets_or_an_unbaked_path_fail() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.dir.path().join("store/ppsspp/share/ppsspp/assets/compat.ini")).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("compat.ini")));

        let fixture = Fixture::new();
        fs::write(fixture.dir.path().join("store/ppsspp/bin/ppsspp"), elf(3, 183)).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("look for its assets elsewhere")));
    }

    #[test]
    fn the_sdl_mapping_must_exist_and_agree_with_the_pad() {
        let fixture = Fixture::new();
        fixture.edit_profile("frontend.toml", "SDL_GAMECONTROLLERCONFIG", "SDL_SOMETHING_ELSE");
        assert!(fixture.failures().iter().any(|failure| failure.contains("SDL_GAMECONTROLLERCONFIG is not set")));

        let fixture = Fixture::new();
        fixture.edit_profile("frontend.toml", "a:b1", "a:b5");
        assert!(fixture.failures().iter().any(|failure| failure.contains("SDL a is")));

        let fixture = Fixture::new();
        fixture.edit_profile("frontend.toml", "1900f6a2", "1900f6a3");
        assert!(fixture.failures().iter().any(|failure| failure.contains("is not the pad's")));

        let fixture = Fixture::new();
        fixture.edit_profile("gamepad.cfg", "input_start_btn = \"9\"", "input_start_btn = \"8\"");
        let failures = fixture.failures();
        assert!(failures.iter().any(|failure| failure.contains("SDL start is")), "{failures:#?}");
    }

    #[test]
    fn a_boot_command_missing_from_the_runtime_fails() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.tree_path("/usr/sbin/sshd")).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("/usr/sbin/sshd")));
    }

    #[test]
    fn a_wrapper_script_is_a_valid_standalone_command_and_an_unknown_launch_kind_is_left_alone() {
        use std::io::Write;
        let fixture = Fixture::new();
        let mut frontend = fs::OpenOptions::new().append(true).open(fixture.dir.path().join("profile/frontend.toml")).unwrap();
        writeln!(frontend, "\n[[system]]\nname = \"Stream\"\ncommand = [\"/usr/bin/stream\", \"{{rom}}\"]\n[[system]]\nname = \"Future\"\nkind = \"something-new\"").unwrap();
        fixture.program(Path::new("/usr/bin/stream"), b"#!/bin/sh\nexec true\n");
        let failures = fixture.failures();
        assert!(failures.is_empty(), "{failures:#?}");
        // A binary that is neither a script nor an aarch64 ELF is still refused.
        fixture.program(Path::new("/usr/bin/stream"), &elf(2, 62));
        assert!(fixture.failures().iter().any(|failure| failure.contains("Stream") && failure.contains("not built for aarch64")));
    }

    #[test]
    fn a_non_executable_emulator_fails() {
        let fixture = Fixture::new();
        fs::set_permissions(fixture.dir.path().join("store/ppsspp/bin/ppsspp"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("PSP") && failure.contains("not executable")));
    }

    #[test]
    fn a_missing_certificate_bundle_fails() {
        let fixture = Fixture::new();
        fs::remove_file(fixture.tree_path("/etc/ssl/certs/ca-certificates.crt")).unwrap();
        assert!(fixture.failures().iter().any(|failure| failure.contains("ca-certificates.crt")));
    }
}

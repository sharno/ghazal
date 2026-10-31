//! Release gates for the device runtime.
//!
//! ```text
//! ghazal-release-check layout TREE [PROFILE]   static checks of the runtime tree
//! ghazal-release-check input  TREE [PROFILE]   headless RetroArch driven by a virtual pad
//! ghazal-release-check all    TREE [PROFILE]   both
//! ```
//!
//! `TREE` is the `runtime` flake output (`nix build .#runtime`); `PROFILE` is
//! the shipped configuration directory, `config/rg40xxv-rocknix` by default.
//! Exit status: 0 all checks passed, 1 a check failed, 77 the input test
//! could not run here (no `/dev/uinput`) and everything else passed.

mod gamepad;
mod layout;
mod profile;
mod retroarch;
mod sdl;
mod tree;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use profile::Profile;

const SKIPPED: u8 = 77;
const DEFAULT_PROFILE: &str = "config/rg40xxv-rocknix";
/// The Game Boy core runs the generated test ROM; `frontend.toml` must name it.
const INPUT_CORE: &str = "gambatte_libretro.so";

/// One printed row: a name, a verdict and the lines under it.
struct Row {
    name: String,
    verdict: Verdict,
    lines: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Skip,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

fn layout_rows(tree: &Path, profile: &Profile) -> Vec<Row> {
    layout::run(&tree::Tree::new(tree), profile)
        .into_iter()
        .map(|check| match check.outcome {
            Ok(detail) => Row { name: check.name.to_owned(), verdict: Verdict::Pass, lines: vec![detail] },
            Err(problems) => Row { name: check.name.to_owned(), verdict: Verdict::Fail, lines: problems },
        })
        .collect()
}

fn input_rows(tree: &Path, profile: &Profile) -> Result<Vec<Row>> {
    if !profile.cores().contains(&INPUT_CORE) {
        bail!("frontend.toml does not list {INPUT_CORE}, which the input test runs");
    }
    let workdir = std::env::temp_dir().join(format!("ghazal-release-check-{}", std::process::id()));
    std::fs::create_dir_all(&workdir)?;
    let outcome = retroarch::run(&retroarch::Options { tree, profile, workdir: &workdir, core: INPUT_CORE });
    if std::env::var_os("GHAZAL_KEEP_WORKDIR").is_some() {
        eprintln!("kept {}", workdir.display());
    } else {
        let _ = std::fs::remove_dir_all(&workdir);
    }
    Ok(match outcome? {
        retroarch::Outcome::Skipped(reason) => vec![Row { name: "headless RetroArch input test".into(), verdict: Verdict::Skip, lines: vec![reason] }],
        retroarch::Outcome::Steps(steps) => steps
            .into_iter()
            .map(|step| match step.result {
                Ok(detail) => Row { name: step.name.to_owned(), verdict: Verdict::Pass, lines: vec![detail] },
                Err(problem) => Row { name: step.name.to_owned(), verdict: Verdict::Fail, lines: problem.lines().map(str::to_owned).collect() },
            })
            .collect(),
    })
}

fn render(rows: &[Row]) -> String {
    rows.iter().map(|row| format!("{}  {}\n{}", row.verdict.label(), row.name, row.lines.iter().map(|line| format!("      {line}\n")).collect::<String>())).collect()
}

/// GitHub shows this on the run's summary page.
fn markdown(rows: &[Row]) -> String {
    let body: String = rows.iter().map(|row| format!("| {} | {} | {} |\n", row.verdict.label(), row.name, row.lines.join("<br>").replace('|', "\\|"))).collect();
    format!("| | Check | Detail |\n|---|---|---|\n{body}")
}

fn exit_status(rows: &[Row]) -> u8 {
    if rows.iter().any(|row| row.verdict == Verdict::Fail) {
        1
    } else if rows.iter().any(|row| row.verdict == Verdict::Skip) {
        SKIPPED
    } else {
        0
    }
}

fn run() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, tree, rest @ ..] = args.as_slice() else {
        bail!("usage: ghazal-release-check layout|input|all TREE [PROFILE]");
    };
    let profile_dir = PathBuf::from(rest.first().map_or(DEFAULT_PROFILE, String::as_str));
    let profile = Profile::load(&profile_dir)?;
    let tree = Path::new(tree);
    if !tree.is_dir() {
        bail!("{} is not a directory", tree.display());
    }
    let rows = match mode.as_str() {
        "layout" => layout_rows(tree, &profile),
        "input" => input_rows(tree, &profile)?,
        "all" => {
            let mut rows = layout_rows(tree, &profile);
            rows.extend(input_rows(tree, &profile)?);
            rows
        }
        other => bail!("unknown mode {other:?}; use layout, input or all"),
    };
    print!("{}", render(&rows));
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(&summary).context("opening the step summary")?;
        writeln!(file, "### ghazal-release-check {mode}\n\n{}", markdown(&rows))?;
    }
    Ok(ExitCode::from(exit_status(&rows)))
}

fn main() -> ExitCode {
    run().unwrap_or_else(|error| {
        eprintln!("ghazal-release-check: {error:#}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(verdict: Verdict) -> Row {
        Row { name: "x".into(), verdict, lines: vec!["detail".into()] }
    }

    #[test]
    fn a_failure_outranks_a_skip_and_a_skip_outranks_a_pass() {
        assert_eq!(exit_status(&[row(Verdict::Pass), row(Verdict::Skip), row(Verdict::Fail)]), 1);
        assert_eq!(exit_status(&[row(Verdict::Pass), row(Verdict::Skip)]), SKIPPED);
        assert_eq!(exit_status(&[row(Verdict::Pass)]), 0);
    }

    #[test]
    fn rows_render_for_the_terminal_and_the_summary() {
        let rows = [Row { name: "a|b".into(), verdict: Verdict::Fail, lines: vec!["one".into(), "two".into()] }];
        assert_eq!(render(&rows), "FAIL  a|b\n      one\n      two\n");
        assert!(markdown(&rows).contains("| FAIL | a|b | one<br>two |"));
    }
}

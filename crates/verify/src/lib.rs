//! Checks a proposal before anyone sees it (spec: VerifyProposal): the
//! project is copied to a scratch directory, the change is written
//! there, and cargo builds and tests it. The developer's own files are
//! never touched (spec: ModelNeverWritesDirectly).
//!
//! The copy is of the working tree, uncommitted changes included: a git
//! worktree would check out HEAD and verify the proposal against code
//! the developer no longer has.

use anyhow::{Context as _, bail};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use walkdir::WalkDir;

/// A cargo run that takes longer than this is stopped: most likely a
/// test that never ends.
const CARGO_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// A copy of the project in a temporary directory, removed when dropped.
pub struct Scratch {
    dir: tempfile::TempDir,
    /// The project's own target directory, shared so dependencies are
    /// not built again for every proposal.
    target_dir: PathBuf,
}

impl Scratch {
    /// Copies the files git would see (tracked and untracked, not
    /// ignored), or, outside git, everything except `target/` and `.git/`.
    pub fn of(project: &Path) -> anyhow::Result<Scratch> {
        let metadata = cargo_metadata::MetadataCommand::new()
            .current_dir(project)
            .no_deps()
            .exec()
            .context("cargo metadata")?;
        if metadata.workspace_root.as_std_path() != project {
            bail!(
                "{} is part of the workspace at {}; proposals can only be verified for a whole workspace for now",
                project.display(),
                metadata.workspace_root
            );
        }
        let dir = tempfile::Builder::new().prefix("skilj-agent-").tempdir()?;
        for rel in files(project)? {
            let from = project.join(&rel);
            let to = dir.path().join(&rel);
            if !from.is_file() {
                continue; // deleted but not yet committed
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", rel.display()))?;
        }
        absolute_outside_paths(project, dir.path())?;
        Ok(Scratch { dir, target_dir: metadata.target_directory.into_std_path_buf() })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Writes `content` to `rel` in the copy.
    pub fn write(&self, rel: &str, content: &str) -> anyhow::Result<()> {
        std::fs::write(self.path().join(rel), content).with_context(|| format!("writing {rel}"))
    }

    /// Runs one test with `cargo test`. `touched` is the file and line
    /// range the proposal wrote, to tell its errors from ones the
    /// project already had.
    pub fn run_test(&self, package: &str, target: &Target, test: &str, touched: &Touched) -> anyhow::Result<Outcome> {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(self.path())
            .env("CARGO_TARGET_DIR", &self.target_dir)
            .args(["test", "--message-format=json", "-p", package]);
        match target {
            Target::Lib => cmd.arg("--lib"),
            Target::Bin(name) => cmd.args(["--bin", name]),
        };
        cmd.args([test, "--", "--color=never"]);
        let output = run(cmd)?;
        Ok(outcome(&output, test, touched))
    }

    /// Keeps the copy instead of removing it, for a look at what failed.
    pub fn keep(self) -> PathBuf {
        self.dir.keep()
    }
}

/// `cargo check` on a scratch copy, e.g. after a mechanical split: the
/// whole workspace must compile, so there is no touched range.
pub fn check(scratch: &Scratch) -> anyhow::Result<CheckOutcome> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(scratch.path())
        .env("CARGO_TARGET_DIR", &scratch.target_dir)
        .args(["check", "--message-format=json", "--color=never"]);
    let output = run(cmd)?;
    Ok(check_outcome(&output))
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    Failed(Vec<String>),
}

fn check_outcome(output: &Output) -> CheckOutcome {
    let mut errors = Vec::new();
    for line in output.stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg["reason"] != "compiler-message" || msg["message"]["level"] != "error" {
            continue;
        }
        // Only errors with spans: a linker error or build script
        // failure lands on stderr instead.
        if msg["message"]["spans"].as_array().is_some_and(|s| !s.is_empty()) {
            errors.push(msg["message"]["rendered"].as_str().unwrap_or_default().trim_end().to_string());
        }
    }
    if !errors.is_empty() {
        return CheckOutcome::Failed(errors);
    }
    if !output.stderr.contains("Finished") && output.stderr.contains("error") {
        return CheckOutcome::Failed(vec![tail(&output.stderr, 40)]);
    }
    CheckOutcome::Passed
}

/// Which crate target a source file is compiled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Lib,
    Bin(String),
}

impl Target {
    /// `src/bin/x.rs` and `src/bin/x/...` are the binary `x`; any other
    /// file under `src/` is taken to be part of the library.
    pub fn of_file(rel_in_package: &str) -> Target {
        match rel_in_package.strip_prefix("src/bin/") {
            Some(rest) => {
                let name = rest.split('/').next().unwrap_or(rest);
                Target::Bin(name.trim_end_matches(".rs").to_string())
            }
            None => Target::Lib,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Touched {
    /// Relative to the project root, like the index's paths.
    pub file: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// The proposal doesn't compile; rustc's messages for it.
    CompileErrors(Vec<String>),
    /// The project doesn't compile even where the proposal didn't touch
    /// it: nothing a retry can fix.
    BrokenElsewhere(Vec<String>),
    /// It compiles, but the test failed; its output.
    TestFailed(String),
    /// It compiles, but cargo ran no test by that name.
    TestNotRun,
}

fn files(project: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let git = Command::new("git")
        .current_dir(project)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = git
        && out.status.success()
    {
        return Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(String::from_utf8_lossy(p).to_string()))
            .collect());
    }
    let mut files = Vec::new();
    let walk = WalkDir::new(project)
        .into_iter()
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some("target" | ".git")));
    for entry in walk {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.path().strip_prefix(project)?.to_path_buf());
        }
    }
    Ok(files)
}

/// A relative `path = "../x"` dependency pointing outside the project
/// would point at nothing from the scratch directory: make it absolute.
fn absolute_outside_paths(project: &Path, scratch: &Path) -> anyhow::Result<()> {
    for entry in WalkDir::new(scratch).into_iter().flatten() {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest = entry.path();
        let rel_dir = manifest.parent().unwrap_or(scratch).strip_prefix(scratch)?;
        let original_dir = project.join(rel_dir);
        let text = std::fs::read_to_string(manifest)?;
        let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("{}", manifest.display()))?;
        if rewrite_paths(doc.as_item_mut(), project, &original_dir) {
            std::fs::write(manifest, doc.to_string())?;
        }
    }
    Ok(())
}

fn rewrite_paths(item: &mut toml_edit::Item, project: &Path, dir: &Path) -> bool {
    let mut changed = false;
    let visit = |key: &str, value: &mut toml_edit::Item| {
        if key == "path"
            && let Some(p) = value.as_str()
            && Path::new(p).is_relative()
        {
            let resolved = normalize(&dir.join(p));
            if !resolved.starts_with(project) {
                *value = toml_edit::value(resolved.to_string_lossy().to_string());
                return true;
            }
            return false;
        }
        rewrite_paths(value, project, dir)
    };
    if let Some(table) = item.as_table_like_mut() {
        for (key, value) in table.iter_mut() {
            changed |= visit(&key, value);
        }
    } else if let Some(array) = item.as_array_of_tables_mut() {
        for table in array.iter_mut() {
            for (key, value) in table.iter_mut() {
                changed |= visit(&key, value);
            }
        }
    } else if let Some(value) = item.as_value_mut()
        && let Some(inline) = value.as_inline_table_mut()
    {
        for (key, value) in inline.iter_mut() {
            let mut as_item = toml_edit::Item::Value(value.clone());
            if visit(&key, &mut as_item) {
                if let toml_edit::Item::Value(v) = as_item {
                    *value = v;
                }
                changed = true;
            }
        }
    }
    changed
}

/// `a/b/../c` → `a/c`, without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

struct Output {
    stdout: String,
    stderr: String,
}

fn run(mut cmd: Command) -> anyhow::Result<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running cargo")?;
    // Read both pipes on threads, so a full pipe can't stall cargo.
    let mut out_pipe = child.stdout.take().expect("piped");
    let mut err_pipe = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() > CARGO_TIMEOUT {
            let _ = child.kill();
            bail!("cargo test took longer than {} minutes and was stopped", CARGO_TIMEOUT.as_secs() / 60);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(Output {
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn outcome(output: &Output, test: &str, touched: &Touched) -> Outcome {
    let mut in_proposal = Vec::new();
    let mut elsewhere = Vec::new();
    let mut test_lines = Vec::new();
    for line in output.stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            test_lines.push(line);
            continue;
        };
        if msg["reason"] != "compiler-message" || msg["message"]["level"] != "error" {
            continue;
        }
        let rendered = msg["message"]["rendered"].as_str().unwrap_or_default().trim_end().to_string();
        let touches = msg["message"]["spans"].as_array().into_iter().flatten().any(|s| {
            s["file_name"].as_str().is_some_and(|f| f == touched.file)
                && s["line_start"].as_u64().is_some_and(|l| {
                    (touched.start as u64..=touched.end as u64).contains(&l)
                })
        });
        if touches {
            in_proposal.push(rendered);
        } else if msg["message"]["spans"].as_array().is_some_and(|s| !s.is_empty()) {
            elsewhere.push(rendered);
        }
    }
    if !in_proposal.is_empty() {
        return Outcome::CompileErrors(in_proposal);
    }
    if !elsewhere.is_empty() {
        return Outcome::BrokenElsewhere(elsewhere);
    }
    let result = |suffix: &str| {
        test_lines.iter().any(|l| {
            l.strip_prefix("test ")
                .and_then(|l| l.strip_suffix(suffix))
                .is_some_and(|path| path == test || path.ends_with(&format!("::{test}")))
        })
    };
    if result(" ... ok") {
        return Outcome::Passed;
    }
    if result(" ... FAILED") {
        return Outcome::TestFailed(failure_output(&test_lines, test));
    }
    // A build failure without JSON spans (a linker error, a build
    // script) lands on stderr.
    if !output.stderr.contains("Finished") && output.stderr.contains("error") {
        return Outcome::BrokenElsewhere(vec![tail(&output.stderr, 40)]);
    }
    Outcome::TestNotRun
}

/// The `---- path::test stdout ----` block of a failed test.
fn failure_output(lines: &[&str], test: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in lines {
        if line.starts_with("---- ") {
            inside = line.contains(&format!("{test} stdout"));
            continue;
        }
        if inside {
            if line.trim() == "failures:" {
                break;
            }
            out.push(*line);
        }
    }
    out.join("\n").trim().to_string()
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touched() -> Touched {
        Touched { file: "src/wallet.rs".into(), start: 10, end: 20 }
    }

    fn error_at(file: &str, line: u64) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "level": "error",
                "rendered": format!("error[E0308]: mismatched types at {file}:{line}"),
                "spans": [{ "file_name": file, "line_start": line }]
            }
        })
        .to_string()
    }

    fn outcome_of(stdout: &str) -> Outcome {
        outcome(&Output { stdout: stdout.into(), stderr: "Finished".into() }, "rejects_overdraft", &touched())
    }

    #[test]
    fn errors_in_the_proposal_are_for_the_model() {
        let stdout = [error_at("src/wallet.rs", 12), error_at("src/other.rs", 3)].join("\n");
        assert!(matches!(outcome_of(&stdout), Outcome::CompileErrors(e) if e.len() == 1));
    }

    #[test]
    fn errors_elsewhere_mean_the_project_is_broken() {
        assert!(matches!(outcome_of(&error_at("src/wallet.rs", 3)), Outcome::BrokenElsewhere(_)));
    }

    #[test]
    fn reads_the_test_result() {
        let ok = "running 1 test\ntest wallet::tests::rejects_overdraft ... ok\n";
        assert_eq!(outcome_of(ok), Outcome::Passed);
        let failed = "running 1 test\ntest wallet::tests::rejects_overdraft ... FAILED\n\nfailures:\n\n\
            ---- wallet::tests::rejects_overdraft stdout ----\nassertion failed: left == right\n\nfailures:\n";
        assert_eq!(outcome_of(failed), Outcome::TestFailed("assertion failed: left == right".into()));
        assert_eq!(outcome_of("running 0 tests\n"), Outcome::TestNotRun);
    }

    #[test]
    fn a_binary_target_is_found_from_its_path() {
        assert_eq!(Target::of_file("src/bin/server.rs"), Target::Bin("server".into()));
        assert_eq!(Target::of_file("src/bin/alerter/main.rs"), Target::Bin("alerter".into()));
        assert_eq!(Target::of_file("src/helpdesk/company.rs"), Target::Lib);
    }

    #[test]
    fn paths_outside_the_project_become_absolute() {
        let mut doc: toml_edit::DocumentMut = r#"
[dependencies]
skilj = { path = "../skilj/skilj" }
local = { path = "crates/local" }

[lib]
path = "src/lib.rs"
"#
        .parse()
        .unwrap();
        let project = Path::new("/home/me/app");
        assert!(rewrite_paths(doc.as_item_mut(), project, project));
        let text = doc.to_string();
        assert!(text.contains(r#"path = "/home/me/skilj/skilj""#), "{text}");
        assert!(text.contains(r#"path = "crates/local""#));
        assert!(text.contains(r#"path = "src/lib.rs""#));
    }
}

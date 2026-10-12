//! What cargo knows about the project: its packages and whether (and on
//! which version) they depend on skilj.

use crate::FileKind;
use cargo_metadata::MetadataCommand;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub(crate) struct Workspace {
    pub packages: Vec<Package>,
    pub skilj_version: Option<String>,
}

pub(crate) struct Package {
    pub name: String,
    pub dir: PathBuf,
}

const SKILJ_CRATES: [&str; 2] = ["skilj", "skilj-core"];

/// The message `index()` fails with for a project without skilj.
pub fn not_a_skilj_project(root: &Path) -> String {
    format!("{} doesn't depend on skilj or skilj-core", root.display())
}

pub(crate) fn read(root: &Path) -> anyhow::Result<Workspace> {
    let metadata = MetadataCommand::new()
        .current_dir(root)
        .no_deps()
        .exec()?;
    let mut packages = Vec::new();
    let mut skilj_version = None;
    let mut uses_skilj = false;
    for package in metadata.workspace_packages() {
        // Pointed at one package of a bigger workspace (skilj-demo inside
        // skilj): only that package.
        let manifest_dir = package.manifest_path.parent().map(|p| p.as_std_path());
        if !manifest_dir.is_some_and(|d| d.starts_with(root)) {
            continue;
        }
        let skilj_dep = package
            .dependencies
            .iter()
            .find(|d| SKILJ_CRATES.contains(&d.name.as_str()));
        if let Some(dep) = skilj_dep {
            uses_skilj = true;
            if skilj_version.is_none() && dep.path.is_none() {
                skilj_version = Some(dep.req.to_string());
            }
        }
        let dir = package
            .manifest_path
            .parent()
            .map(|p| p.as_std_path().to_path_buf())
            .unwrap_or_else(|| root.to_path_buf());
        packages.push(Package { name: package.name.to_string(), dir });
    }
    if !uses_skilj {
        anyhow::bail!(not_a_skilj_project(root));
    }
    Ok(Workspace { packages, skilj_version })
}

/// The package's `src/` and `tests/` Rust files and its `*.skilj.toml`
/// files, sorted so the index is stable.
pub(crate) fn source_files(package: &Package) -> Vec<(PathBuf, FileKind)> {
    let mut files = Vec::new();
    for (dir, rust_kind) in [("src", FileKind::Src), ("tests", FileKind::Tests)] {
        let dir = package.dir.join(dir);
        for entry in WalkDir::new(&dir).sort_by_file_name().into_iter().flatten() {
            let path = entry.path();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if !entry.file_type().is_file() {
                continue;
            }
            if name.ends_with(".skilj.toml") {
                files.push((path.to_path_buf(), FileKind::SkiljToml));
            } else if name.ends_with(".rs") {
                files.push((path.to_path_buf(), rust_kind));
            }
        }
    }
    files
}

//! Starts a new skilj project (spec: StartNewProject): copies the
//! bundled `templates/skilj-template`, names the package, and renames
//! the `wallet` example context to the developer's first context. No
//! model involved.

use anyhow::{Context as _, bail};
use skilj_agent_checks::LayoutStyle;
use std::path::Path;

/// A bundled template file, relative to `templates/skilj-template`.
const TEMPLATE_FILES: [&str; 7] = [
    "Cargo.toml",
    "README.md",
    "Dockerfile",
    ".dockerignore",
    "src/lib.rs",
    "src/wallet.rs",
    "src/bin/server.rs",
];

macro_rules! template {
    ($path:literal) => {
        include_str!(concat!("../../../templates/skilj-template/", $path))
    };
}

/// The bundled file contents in `TEMPLATE_FILES` order.
fn template_contents() -> [&'static str; 7] {
    [
        template!("Cargo.toml"),
        template!("README.md"),
        template!("Dockerfile"),
        template!(".dockerignore"),
        template!("src/lib.rs"),
        template!("src/wallet.rs"),
        template!("src/bin/server.rs"),
    ]
}

/// Creates the project at `path`: `name` names the package (and the
/// crate), `context` names the first bounded context (renamed from the
/// template's `wallet`), `style` is remembered in `.skilj-agent.toml`.
pub fn create(path: &Path, name: &str, context: &str, style: LayoutStyle) -> anyhow::Result<()> {
    check_name(name)?;
    check_name(context)?;
    if path.exists() && path.read_dir()?.next().is_some() {
        bail!("{} exists and is not empty", path.display());
    }
    let crate_name = name.replace('-', "_");
    let context_snake = context.replace('-', "_").to_lowercase();
    let context_title = to_title(&context_snake);

    let mut renames: Vec<(String, String)> = Vec::new();
    for (rel, content) in TEMPLATE_FILES.iter().zip(template_contents()) {
        let rel = rel
            .replace("wallet.rs", &format!("{context_snake}.rs"))
            .replace("wallet", &context_snake);
        let out = content
            .replace("{{project-name}}", name)
            .replace("{{crate_name}}", &crate_name)
            .replace("WalletEvent", &format!("{context_title}Event"))
            .replace("wallet_tag", &format!("{context_snake}_tag"))
            .replace("wallet_id", &format!("{context_snake}_id"))
            .replace("wallet", &context_snake);
        let dest = path.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, out).with_context(|| format!("writing {}", dest.display()))?;
        renames.push((rel, String::new()));
    }
    drop(renames);

    let wallet_rs = path.join(format!("src/{context_snake}.rs"));
    if !wallet_rs.is_file() {
        bail!("template did not produce src/{context_snake}.rs");
    }
    let config = match style {
        LayoutStyle::ByKind => "by_kind",
        LayoutStyle::BySlice => "by_slice",
        LayoutStyle::CratePerContext => "crate_per_context",
    };
    std::fs::write(
        path.join(".skilj-agent.toml"),
        format!("# Chosen by `skilj-agent new`; see `skilj-agent check`.\nlayout_style = \"{config}\"\n"),
    )?;
    Ok(())
}

fn check_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    if !ok {
        bail!("{name:?} is not a valid project or context name (letters, digits, - and _; must start with a letter)");
    }
    Ok(())
}

fn to_title(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_tmp(name: &str, context: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        create(&dir.path().join("app"), name, context, LayoutStyle::BySlice).unwrap();
        dir
    }

    #[test]
    fn renames_the_crate_and_context() {
        let dir = create_tmp("my-shop", "orders");
        let root = dir.path().join("app");
        assert!(root.join("src/orders.rs").is_file());
        assert!(!root.join("src/wallet.rs").exists());
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(manifest.contains("name = \"my-shop\""), "{manifest}");
        assert!(!manifest.contains("{{"), "{manifest}");
        let lib = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(lib.contains("pub mod orders;"), "{lib}");
        let ctx = std::fs::read_to_string(root.join("src/orders.rs")).unwrap();
        assert!(ctx.contains("pub const BOUNDED_CONTEXT: &str = \"orders\";"), "{ctx}");
        assert!(ctx.contains("OrdersEvent"), "{ctx}");
        assert!(!ctx.contains("wallet"), "{ctx}");
        let config = std::fs::read_to_string(root.join(".skilj-agent.toml")).unwrap();
        assert!(config.contains("layout_style = \"by_slice\""), "{config}");
    }

    #[test]
    fn refuses_bad_names_and_non_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create(&dir.path().join("a"), "bad name!", "ok", LayoutStyle::ByKind).is_err());
        assert!(create(&dir.path().join("b"), "ok", "", LayoutStyle::ByKind).is_err());
        let full = dir.path().join("c");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("x"), "y").unwrap();
        assert!(create(&full, "ok", "ok", LayoutStyle::ByKind).is_err());
    }
}

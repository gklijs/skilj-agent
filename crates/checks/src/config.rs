//! `.skilj-agent.toml` in the project root: the team decisions that are
//! committed with the project (layout style, dismissed findings).

use crate::{FindingKind, LayoutStyle};
use serde::Deserialize;
use std::path::Path;

pub const FILE_NAME: &str = ".skilj-agent.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub layout_style: Option<LayoutStyle>,
    #[serde(default)]
    pub dismissed: Vec<Dismissal>,
}

/// A finding dismissed as not relevant; it stays dismissed when the
/// findings are recomputed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dismissal {
    pub kind: FindingKind,
    pub subject: String,
}

impl ProjectConfig {
    /// Reads the project's config; a missing file is an empty config.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_style_and_dismissals() {
        let config: ProjectConfig = toml::from_str(
            r#"
            layout_style = "by_slice"

            [[dismissed]]
            kind = "too_big"
            subject = "src/routing_guard.rs"
            "#,
        )
        .unwrap();
        assert_eq!(config.layout_style, Some(LayoutStyle::BySlice));
        assert_eq!(config.dismissed[0].kind, FindingKind::TooBig);
    }
}

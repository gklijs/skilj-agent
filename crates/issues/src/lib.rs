//! Suggested issues on the skilj-agent repo (spec: IssueSuggestion,
//! MatchExistingIssue, ForgeUnreachable, RetryQueuedIssues, FileIssue).
//!
//! Every failure of the agent becomes a suggestion, so what local models
//! can't do yet turns into knowledge the agent ships. A suggestion never
//! holds project code: the text is built from fixed categories and
//! versions, and the developer sees (and can edit) all of it before
//! anything is posted.
//!
//! Matching is by key: each suggestion has one, like
//! `write-test/gave-up/command/test-failed`, written into the issue it
//! opens. An issue whose body holds the same key line is the same gap,
//! and gets a comment with this occurrence instead of a new issue. The
//! repo's issues are listed rather than searched, since search lags
//! behind new issues.
//!
//! Filed with `gh` when it is logged in; otherwise through the new-issue
//! page, pre-filled. Suggestions that can't be offered yet (no terminal,
//! GitHub unreachable) wait in a pending file, merged by key.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const REPO: &str = "gklijs/skilj-agent";
const KEY_PREFIX: &str = "skilj-agent key: ";
/// Pages of 100 issues listed when looking for a match.
const MAX_PAGES: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// What gap this is, the same for every occurrence of it.
    pub key: String,
    pub title: String,
    /// What happened, in categories: never project code.
    pub summary: String,
    /// One line per occurrence: versions, model, outcome.
    pub occurrences: Vec<String>,
}

impl Suggestion {
    pub fn new(key: impl Into<String>, title: impl Into<String>, summary: impl Into<String>, occurrence: Occurrence) -> Self {
        Self { key: key.into(), title: title.into(), summary: summary.into(), occurrences: vec![occurrence.line()] }
    }

    /// The body of a new issue: the key line at the end, so the next
    /// occurrence finds it.
    pub fn issue_body(&self) -> String {
        format!(
            "{}\n\n{}\n\n<!-- The agent finds this issue again by the line below; please keep it. -->\n{KEY_PREFIX}`{}`\n",
            self.summary,
            self.occurrence_list(),
            self.key
        )
    }

    /// The body of a comment on an existing issue for the same gap.
    pub fn comment_body(&self) -> String {
        format!("Seen again.\n\n{}\n", self.occurrence_list())
    }

    fn occurrence_list(&self) -> String {
        self.occurrences.iter().map(|o| format!("- {o}")).collect::<Vec<_>>().join("\n")
    }

    /// Adds another occurrence's lines, keeping each distinct one once.
    fn merge(&mut self, other: &Suggestion) {
        for o in &other.occurrences {
            if !self.occurrences.contains(o) {
                self.occurrences.push(o.clone());
            }
        }
    }
}

/// What is known about one occurrence: nothing from the project but its
/// skilj version requirement.
#[derive(Debug, Clone, Default)]
pub struct Occurrence {
    pub skilj: Option<String>,
    pub model: Option<String>,
    pub outcome: Option<String>,
}

impl Occurrence {
    fn line(&self) -> String {
        let mut parts = vec![format!("skilj-agent {}", env!("CARGO_PKG_VERSION"))];
        if let Some(s) = &self.skilj {
            parts.push(format!("skilj {s}"));
        }
        if let Some(m) = &self.model {
            parts.push(m.clone());
        }
        if let Some(o) = &self.outcome {
            parts.push(o.clone());
        }
        parts.join(", ")
    }
}

/// An open or closed issue describing the same gap.
#[derive(Debug, Clone, PartialEq)]
pub struct Existing {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub open: bool,
}

#[derive(Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    html_url: String,
    state: String,
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
}

/// The issue whose key line is `key`, if any. Anonymous: the repo is
/// public. An error means GitHub couldn't be asked.
pub async fn find_existing(key: &str) -> anyhow::Result<Option<Existing>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("skilj-agent/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    for page in 1..=MAX_PAGES {
        let url = format!("https://api.github.com/repos/{REPO}/issues?state=all&per_page=100&page={page}");
        let response = client.get(&url).header("Accept", "application/vnd.github+json").send().await?;
        if !response.status().is_success() {
            bail!("GitHub answered {} listing the issues", response.status());
        }
        let issues: Vec<ApiIssue> = response.json().await?;
        if let Some(found) = match_key(&issues, key) {
            return Ok(Some(found));
        }
        if issues.len() < 100 {
            break;
        }
    }
    Ok(None)
}

fn match_key(issues: &[ApiIssue], key: &str) -> Option<Existing> {
    let line = format!("{KEY_PREFIX}`{key}`");
    issues
        .iter()
        .filter(|i| i.pull_request.is_none())
        .find(|i| i.body.as_deref().is_some_and(|b| b.lines().any(|l| l.trim() == line)))
        .map(|i| Existing { number: i.number, title: i.title.clone(), url: i.html_url.clone(), open: i.state == "open" })
}

/// How a suggestion was filed.
#[derive(Debug)]
pub enum Filed {
    /// Posted with `gh`; the issue or comment URL.
    Posted(String),
    /// No `gh` login: the page to post it from. For a comment, the text
    /// still has to be pasted.
    InBrowser(String),
}

/// The title and text a suggestion is filed with: a new issue, or a
/// comment on `existing`. The developer may edit both before filing.
pub fn draft(suggestion: &Suggestion, existing: Option<&Existing>) -> (String, String) {
    match existing {
        Some(_) => (suggestion.title.clone(), suggestion.comment_body()),
        None => (suggestion.title.clone(), suggestion.issue_body()),
    }
}

/// Opens the issue, or comments on `existing`, with exactly this text.
/// Only call this after the developer approved it.
pub fn file(title: &str, body: &str, existing: Option<&Existing>) -> anyhow::Result<Filed> {
    if gh_logged_in() {
        let number = existing.map(|e| e.number.to_string());
        let mut args = vec![];
        match &number {
            Some(n) => args.extend(["issue", "comment", n.as_str()]),
            None => args.extend(["issue", "create", "--title", title]),
        }
        args.extend(["--repo", REPO, "--body-file", "-"]);
        let url = run_with_input("gh", &args, body)?;
        return Ok(Filed::Posted(url.trim().to_string()));
    }
    let url = match existing {
        Some(e) => e.url.clone(),
        None => format!(
            "https://github.com/{REPO}/issues/new?title={}&body={}",
            encode(title),
            encode(body)
        ),
    };
    // Best effort: the URL is shown either way.
    for opener in ["xdg-open", "wslview", "open"] {
        if Command::new(opener).arg(&url).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
            break;
        }
    }
    Ok(Filed::InBrowser(url))
}

fn gh_logged_in() -> bool {
    Command::new("gh")
        .args(["auth", "status", "--hostname", "github.com"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_with_input(program: &str, args: &[&str], input: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {program}"))?;
    child.stdin.take().expect("piped").write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Percent-encoding for a query value.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Suggestions waiting to be offered, per user: they hold no project
/// code. Merged by key, so a gap hit ten times is one suggestion with
/// its occurrences.
pub struct Pending {
    path: PathBuf,
}

impl Pending {
    /// `$XDG_CONFIG_HOME/skilj-agent/pending-issues.json`, or under
    /// `~/.config`.
    pub fn for_user() -> anyhow::Result<Self> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var_os("HOME").context("no HOME to keep pending issues in")?).join(".config"),
        };
        Ok(Self::at(base.join("skilj-agent").join("pending-issues.json")))
    }

    pub fn at(path: impl AsRef<Path>) -> Self {
        Self { path: path.as_ref().to_path_buf() }
    }

    pub fn load(&self) -> anyhow::Result<Vec<Suggestion>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| self.path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Adds `suggestion`, merging it into a pending one with its key.
    pub fn add(&self, suggestion: &Suggestion) -> anyhow::Result<()> {
        let mut all = self.load()?;
        match all.iter_mut().find(|s| s.key == suggestion.key) {
            Some(s) => s.merge(suggestion),
            None => all.push(suggestion.clone()),
        }
        self.save(&all)
    }

    /// Drops the pending suggestion with `key`: filed or declined.
    pub fn remove(&self, key: &str) -> anyhow::Result<()> {
        let mut all = self.load()?;
        all.retain(|s| s.key != key);
        self.save(&all)
    }

    fn save(&self, all: &[Suggestion]) -> anyhow::Result<()> {
        if all.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(all)?).with_context(|| self.path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(outcome: &str) -> Suggestion {
        Suggestion::new(
            "write-test/gave-up/command/test-failed",
            "write-test: gave up on a command obligation",
            "write-test gave up: the test failed every attempt.",
            Occurrence { skilj: Some("0.0.10".into()), model: Some("qwen3:14b".into()), outcome: Some(outcome.into()) },
        )
    }

    #[test]
    fn a_new_issue_ends_with_its_key_line_and_a_comment_has_none() {
        let s = suggestion("3 attempts");
        let body = s.issue_body();
        assert!(body.starts_with("write-test gave up: the test failed every attempt.\n\n- skilj-agent "), "{body}");
        assert!(body.contains(", skilj 0.0.10, qwen3:14b, 3 attempts\n"), "{body}");
        assert!(body.ends_with("skilj-agent key: `write-test/gave-up/command/test-failed`\n"), "{body}");
        assert!(!s.comment_body().contains("key"));
    }

    #[test]
    fn matches_an_issue_by_its_key_line_only() {
        let issue = |number, body: &str, pr| ApiIssue {
            number,
            title: format!("#{number}"),
            html_url: format!("https://github.com/{REPO}/issues/{number}"),
            state: "closed".into(),
            body: Some(body.into()),
            pull_request: pr,
        };
        let key = "write-test/gave-up/command/test-failed";
        let issues = vec![
            issue(1, "mentions write-test/gave-up/command/test-failed in passing", None),
            issue(2, "skilj-agent key: `write-test/gave-up/command/test-failed`", Some(serde_json::json!({}))),
            issue(3, "text\nskilj-agent key: `write-test/gave-up/command/test-failed`\n", None),
        ];
        let found = match_key(&issues, key).unwrap();
        assert_eq!((found.number, found.open), (3, false));
        assert!(match_key(&issues, "write-test/gave-up/command/cut-off").is_none());
    }

    #[test]
    fn pending_suggestions_merge_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let pending = Pending::at(dir.path().join("sub/pending-issues.json"));
        assert!(pending.load().unwrap().is_empty());
        pending.add(&suggestion("3 attempts")).unwrap();
        pending.add(&suggestion("3 attempts")).unwrap();
        pending.add(&suggestion("cut off")).unwrap();
        let all = pending.load().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].occurrences.len(), 2);
        pending.remove(&all[0].key).unwrap();
        assert!(!dir.path().join("sub/pending-issues.json").exists());
    }

    #[test]
    fn encodes_a_query_value() {
        assert_eq!(encode("a b/c\n`d`"), "a%20b%2Fc%0A%60d%60");
    }
}

#[cfg(test)]
mod against_github {
    /// Reads the real repo's issues, anonymously. `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn finds_no_issue_for_a_key_nobody_used() {
        assert_eq!(super::find_existing("test/no-such-key").await.unwrap(), None);
    }
}

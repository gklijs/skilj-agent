//! Offering suggested issues: look for the same gap on GitHub, show the
//! exact text, and only file it on a yes.

use skilj_agent_issues::{Existing, Filed, Pending, REPO, Suggestion};
use std::io::IsTerminal as _;

enum Answer {
    Filed,
    Declined,
    Later,
}

/// Offers `suggestion` now when there is a terminal to ask on; otherwise
/// keeps it for `skilj-agent issues`.
pub fn suggest(suggestion: Suggestion) -> anyhow::Result<()> {
    let pending = Pending::for_user()?;
    if !std::io::stdin().is_terminal() {
        pending.add(&suggestion)?;
        eprintln!("Kept as a suggested issue on {REPO} (no project code in it): `skilj-agent issues` offers it.");
        return Ok(());
    }
    eprintln!();
    if let Answer::Later = offer(&suggestion)? {
        pending.add(&suggestion)?;
    }
    Ok(())
}

/// `skilj-agent issues`: offers each pending suggestion again.
pub fn run_pending() -> anyhow::Result<()> {
    let pending = Pending::for_user()?;
    let all = pending.load()?;
    if all.is_empty() {
        eprintln!("No suggested issues waiting.");
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        for s in &all {
            eprintln!("{} ({} occurrence{})", s.title, s.occurrences.len(), if s.occurrences.len() == 1 { "" } else { "s" });
        }
        eprintln!("Run `skilj-agent issues` in a terminal to file or decline them.");
        return Ok(());
    }
    for (i, s) in all.iter().enumerate() {
        eprintln!("--- {} of {}", i + 1, all.len());
        match offer(s)? {
            Answer::Filed | Answer::Declined => pending.remove(&s.key)?,
            Answer::Later => {}
        }
    }
    Ok(())
}

fn offer(suggestion: &Suggestion) -> anyhow::Result<Answer> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let existing = match runtime.block_on(skilj_agent_issues::find_existing(&suggestion.key)) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("Couldn't look for a matching issue on {REPO} ({e}); kept it for `skilj-agent issues`.");
            return Ok(Answer::Later);
        }
    };
    let (mut title, mut body) = skilj_agent_issues::draft(suggestion, existing.as_ref());
    loop {
        show(&title, &body, existing.as_ref());
        eprint!("File it? [y]es, [e]dit, [l]ater, [N]o: ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        match answer.trim() {
            "y" | "Y" | "yes" => {
                match skilj_agent_issues::file(&title, &body, existing.as_ref())? {
                    Filed::Posted(url) => eprintln!("Posted: {url}"),
                    Filed::InBrowser(url) if existing.is_some() => {
                        eprintln!("No gh login: add the text above as a comment on {url}");
                    }
                    Filed::InBrowser(url) => eprintln!("No gh login: post it from this page, pre-filled:\n{url}"),
                }
                return Ok(Answer::Filed);
            }
            "e" | "E" | "edit" => (title, body) = edit(&title, &body, existing.is_none())?,
            "l" | "L" | "later" => return Ok(Answer::Later),
            _ => return Ok(Answer::Declined),
        }
    }
}

fn show(title: &str, body: &str, existing: Option<&Existing>) {
    match existing {
        Some(e) => eprintln!(
            "Suggested comment on {REPO}#{} \"{}\" ({}), the same gap:",
            e.number,
            e.title,
            if e.open { "open" } else { "closed" }
        ),
        None => eprintln!("Suggested new issue on {REPO}: {title}"),
    }
    eprintln!();
    for line in body.lines() {
        eprintln!("    {line}");
    }
    eprintln!();
}

/// Lets the developer change the text in `$EDITOR`; for a new issue the
/// first line is the title.
fn edit(title: &str, body: &str, with_title: bool) -> anyhow::Result<(String, String)> {
    let file = tempfile::Builder::new().prefix("skilj-agent-issue-").suffix(".md").tempfile()?;
    let text = if with_title { format!("{title}\n\n{body}") } else { body.to_string() };
    std::fs::write(file.path(), text)?;
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(file.path()).status()?;
    if !status.success() {
        anyhow::bail!("{editor} exited with {status}; nothing changed");
    }
    let edited = std::fs::read_to_string(file.path())?;
    if !with_title {
        return Ok((title.to_string(), edited));
    }
    let (first, rest) = edited.split_once('\n').unwrap_or((&edited, ""));
    Ok((first.trim().to_string(), rest.trim_start_matches('\n').to_string()))
}

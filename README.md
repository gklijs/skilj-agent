# skilj-agent

An agent for applications built on [SkilJ](https://codeberg.org/gklijs/SkilJ), designed to work
with local models (Ollama). Everything that's the same in every SkilJ project is plain Rust code;
a model only does the parts that need judgement or new text. See
[`specs/skilj-agent.allium`](specs/skilj-agent.allium) for the behaviour and
[`docs/design.md`](docs/design.md) for the reasoning.

`check` and `diagnose` use no model. `write-test` is the first command that does.

## `skilj-agent new`

```sh
skilj-agent new ../my-shop --name my-shop --context orders --style by_slice
```

Starts a new skilj project from the bundled template (a working
deposit/withdraw `wallet` app with a runnable server): names the
package, renames the example context to the first context, and
remembers the layout style in `.skilj-agent.toml`. No model involved.

## `skilj-agent check`

```sh
cargo run -p skilj-agent -- check ../skilj-helpdesk
cargo run -p skilj-agent -- check ../skilj-helpdesk --all    # every finding
cargo run -p skilj-agent -- check ../skilj-helpdesk --json   # for scripts and other agents
```

It reads the project and reports:

- **Bounded contexts** and their events, commands, projections and snapshots, from
  `#[auto_register]` impls and skilj-codegen's `*.skilj.toml` files.
- **What's untested.** For every command: a unit test of `decide()` for the accepted path and for
  every rejection kind, plus one integration test. For every projection: a unit test of
  `project()` for every event it consumes, plus one integration test. Rejection kinds are found by
  following `decide()` into the helpers it calls. When the project has `specs/*.allium`, the
  obligations from `allium plan` are added too.
- **Layout:** how each context is laid out, files above 800 lines, files mixing contexts, and,
  once a style is chosen, contexts that don't follow it.
- **The skilj version**, compared with the one this release knows.

The same project always gives the same report.

### How tests are matched

- A **unit test** is a `#[test]` under `src/`. It covers a command branch when it calls that
  command's `decide()` (directly or through a helper in the test module) and mentions the
  rejection kind, or no rejection kind for the accepted path. For a projection it calls
  `project()` and mentions the event.
- An **integration test** is a `#[test]` under `tests/` that mentions the command or projection
  by its name.
- When the matching misses a test, mark it:

  ```rust
  #[test]
  fn withdrawing_more_than_the_balance() {
      // skilj-agent: covers command.Withdraw.rejected.insufficient_funds
      ...
  }
  ```

  For Allium spec obligations use `// allium: <id>` with the id from `allium plan`.

### `.skilj-agent.toml`

Optional, in the project root, committed: decisions for the whole team.

```toml
# by_kind, by_slice or crate_per_context
layout_style = "by_slice"

# Findings that are not relevant here, or wrong. They stay dismissed, and
# write-test refuses a dismissed obligation.
[[dismissed]]
kind = "too_big"
subject = "src/helpdesk/ticket_commands.rs"
reason = "one file per aggregate, on purpose"   # optional
```

A rejection a command only reaches through another command's logic (`ApplyMacro` calling
`RequestInfoFromCustomer::decide_on`) is reported with "only reached through ..., so it may be
unreachable from here": the index sees the call, not the arguments passed.

## `skilj-agent diagnose`

```sh
cargo test 2>&1 | skilj-agent diagnose      # or: skilj-agent diagnose server.log
skilj-agent diagnose server.log --json
```

Recognises skilj errors in any output (a failing test, server logs, a REST or GraphQL response)
by their wire code (`invalid_tag_mapping`), their message, or their variant name
(`EventStore(SchemaIncompatible)`), and shows the recorded cause and fix. An error without a
recorded fix is still named, with skilj's own message. No model is involved.

## `skilj-agent write-test`

```sh
skilj-agent write-test command.Withdraw.rejected.insufficient_funds --path ../my-app
skilj-agent write-test projection.Balance.folds.Deposited --show-prompt   # what the model gets
skilj-agent write-test ... --model qwen3:14b --transcript run.txt
```

Writes one unit test for one untested obligation from `check --all` (the accepted path or a
rejection of a command, or a projection folding an event). Code decides everything around the
test, and the model writes only the test function:

- **What the model sees:** the type's own code and what it refers to, followed by name through the
  package (an event enum only brings the payloads of the variants that are used), an existing
  unit test from the project to imitate, and `knowledge/testing.md`.
- **Where it goes:** the test module the example is in, else the file's first `#[cfg(test)]`
  module, else a new one at the end of the file. For skilj-codegen types, the hand-written `.rs`
  next to the `.skilj.toml`.
- **How it's checked:** in a scratch copy of the project (the working tree, uncommitted changes
  included, built in the project's own `target/` so dependencies aren't rebuilt):
  1. the answer must be one `#[test]` function with a free name;
  2. `check`'s own matching must count it as covering the obligation;
  3. `cargo test` must compile and pass it.
  What fails goes back to the model, three attempts in all.
- **Your files** are only changed when you confirm, or with `--apply`, and only if the file hasn't
  changed in the meantime.

When it gives up on a rejection that is inherited like that, or every test that ran was
rejected with another of the command's kinds, it says the rejection may be unreachable and
prints the `[[dismissed]]` entry to add. It never dismisses anything itself.

## `skilj-agent split-context`

```sh
skilj-agent split-context helpdesk --path ../skilj-helpdesk
skilj-agent split-context helpdesk --path ../skilj-helpdesk --style by_kind --apply
```

Splits one single-file bounded context into the shape the project's
layout style prescribes (`by_kind`: `events.rs` + `commands.rs` +
`projections.rs`; `by_slice`: one file per command/projection, shared
events and helpers in `events.rs`). Mechanical, no model: items move
with `syn`, shared helpers are imported from `events.rs`, the result
is `cargo check`ed in a scratch copy, and your files only change when
you confirm (or with `--apply`). Refused with a reason for
`crate_per_context`, codegen contexts, and contexts sharing files.

`scripts/eval-write-test.py <project> results.jsonl [--no-think]` runs it over every untested unit
obligation and records each outcome, for comparing models and changes.

The model is `qwen3:14b` on Ollama by default (`--model`, `SKILJ_AGENT_MODEL`, `OLLAMA_HOST`),
with a 16k context and at most 8k tokens and 6 minutes per answer. `--thinking` sets when it
reasons first: `fallback` (the default) answers once without, which is enough for simple cases
in a fraction of the time, then retries with thinking, three attempts of its own; `always`;
`never` (or `--no-think`).

## Suggested issues

Whatever the agent can't do, or does wrong, becomes a suggested issue on
[gklijs/skilj-agent](https://github.com/gklijs/skilj-agent/issues), so it can learn it: `write-test`
giving up or proposing a wrong test, `diagnose` not recognising skilj output or knowing no fix.

- **Never project code:** the text is built from fixed categories, versions and the model's name.
- **One issue per gap:** each suggestion has a key (`write-test/gave-up/command/test-failed`) that
  is written into the issue. When an issue with that key exists, open or closed, the suggestion
  becomes a comment on it with this occurrence.
- **Only with your yes:** you see the exact text, and can edit it, before anything is posted.
  Posted with `gh` when it's logged in, otherwise from a pre-filled GitHub page.
- **Later:** without a terminal (`cargo test | skilj-agent diagnose`), or when GitHub can't be
  reached, it's kept in `~/.config/skilj-agent/pending-issues.json`, merged by key.
  `skilj-agent issues` offers what's waiting.

## Knowledge

| File | What it is |
|---|---|
| `knowledge/skilj-errors.json` | Every skilj-core error variant with its code and message. Generated: `scripts/extract-skilj-errors.py ../skilj > knowledge/skilj-errors.json` |
| `knowledge/testing.md` | How to unit test `decide()`, `decide_with()` and `project()`: what `write-test` gives the model. |
| `knowledge/error-fixes.toml` | Causes and fixes, written by hand, seeded from skilj's `common-mistakes.md`. Add to it when an issue teaches a fix. |

All are compiled into the binary. A test checks that every fix names a variant that exists.

## Layout

| Crate | What it does |
|---|---|
| `crates/index` | Reads a project: contexts, plugin types, tests. No model. |
| `crates/checks` | Turns the index into obligations and findings. No model. |
| `crates/verify` | Scratch copies and `cargo test` runs, with errors sorted into the proposal's own and the project's. |
| `crates/llm` | The model, through rig: Ollama for now. |
| `crates/tasks` | Tasks that use a model: `write_test`. |
| `crates/issues` | Suggested issues: matching by key, filing with `gh` or the browser, pending ones. |
| `crates/new` | `new`: the bundled skilj-template plus the crate/context rename. No model. |
| `crates/split` | `split-context`: the mechanical syn-based context split. No model. |
| `crates/cli` | The `skilj-agent` binary. |

`crates/checks/tests/fixtures/wallet` is a small project with known coverage, used by the
end-to-end tests.

# skilj-agent

An agent for applications built on [SkilJ](https://codeberg.org/gklijs/SkilJ), designed to work
with local models (Ollama). Everything that's the same in every SkilJ project is plain Rust code;
a model only does the parts that need judgement or new text. See
[`specs/skilj-agent.allium`](specs/skilj-agent.allium) for the behaviour and
[`docs/design.md`](docs/design.md) for the reasoning.

So far only the part without a model exists: `check`.

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

# Findings that are correct but not relevant here. They stay dismissed.
[[dismissed]]
kind = "too_big"
subject = "src/helpdesk/ticket_commands.rs"
```

## Layout

| Crate | What it does |
|---|---|
| `crates/index` | Reads a project: contexts, plugin types, tests. No model. |
| `crates/checks` | Turns the index into obligations and findings. No model. |
| `crates/cli` | The `skilj-agent` binary. |

`crates/checks/tests/fixtures/wallet` is a small project with known coverage, used by the
end-to-end tests.

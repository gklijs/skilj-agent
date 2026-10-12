# skilj-agent: design notes

*Started 2026-10-12. The behaviour is in [`specs/skilj-agent.allium`](../specs/skilj-agent.allium);
this file covers the reasoning behind it.*

## The idea in one paragraph

An interactive Rust CLI, built on [rig](https://rig.rs), that helps people work on applications built on
SkilJ. Most of what such an agent needs to know is the same for every SkilJ project: where the
bounded contexts are, what a command or projection looks like, how to test one, what each
`skilj_core` error means. That part becomes ordinary Rust code. The model only does the parts
that need judgement or new text, inside a loop where the compiler and the tests check its work.
That's what makes the results predictable, and it's also what makes a 14B local model good
enough.

Anything the agent can't do, or does badly, becomes a suggested issue on the skilj-agent repo
(or a comment on a matching existing one). Over time that builds collective knowledge about
writing SkilJ applications with local models.

## Can we still use skills with Ollama?

Not as such, but you don't lose much. A skill is a convention of Claude Code and the Agent SDK:
markdown files the model loads on demand. rig doesn't have that mechanism, and small local
models are bad at deciding *when* to load something anyway.

What carries over is the **content**. The skilj skill's reference files (`event-type.md`,
`command-type.md`, `projection.md`, `common-mistakes.md`, ...) become `include_str!` text that
the agent itself picks per task:

| Task | Context the code selects | Model's job |
|---|---|---|
| Write a test for `Withdraw` rejected with `insufficient_funds` | the `Withdraw` impl, `command-type.md` testing section, one existing test in the project | write one test function |
| Explain a performance finding | the finding, the matching section of `performance.md` | explain it in plain words |
| Unknown error | the error output, the files it points at | propose a fix |

`common-mistakes.md` stops being prompt text altogether: it becomes a lookup table from error
variant to cause and fix. No model needed.

The skills don't have to be thrown away either: the same text can stay a Claude Code skill
for people who use Claude Code. Open question: copy the text into this repo or read it from the
skilj repo at build time.

## Code versus model

| Capability | Deterministic (Rust) | Model |
|---|---|---|
| Open project | `cargo metadata`; `syn` parse for `BOUNDED_CONTEXT` consts and `#[auto_register]` impls; rejection kinds from `decide()` | — |
| New project | `cargo generate` on `templates/skilj-template`, rename the `wallet` context | maybe: suggest first events/commands from a description |
| Is it tested? | obligations from the index + `allium plan` when the project has specs; match against tests | — |
| Add tests | pick context, write file, `cargo test` in a scratch copy, feed errors back (max 3) | write the test body |
| Ordering | detect the layout style (by kind, by slice, crate per context), flag contexts that don't follow it; restructure with `syn` + `prettyplease` | — |
| Performance | fixed checks from skilj's `docs/performance.md` | explain, prioritise |
| Debugging | error lookup table | anything not in the table |

Two things from your other projects fit well here:

- **`allium plan`** already produces stable obligation ids (`rule-success.X`,
  `entity-fields.Y`, ...). skilj-helpdesk has three specs. "Is every rule tested?" becomes a
  set difference, given a convention such as an `// allium: <id>` comment on the test.
- **skilj-helpdesk's `helpdesk` context** (3290 lines over `src/helpdesk/`, two files above
  800 lines) is a good real test case for the layout check and the split.

## Making local models work

- **One narrow task per call**, with the context already chosen. No open-ended tool browsing.
- **Structured output** through rig's `Extractor` (a JSON Schema per answer type) wherever the
  answer isn't code, so it never has to be parsed out of prose.
- **Few tools per step.** If a step needs tools, two or three, not twenty.
- **The compiler is the reviewer.** Every proposal is built and tested in a scratch copy of the working tree
  before you see it.
- **The provider is configuration.** rig has Ollama, OpenAI-compatible (also llama.cpp,
  LM Studio, vLLM) and Anthropic clients. Ollama is the default. `my-rig-agent` already talks to
  Ollama with rig 0.40. Locally there's `qwen3:14b`.

## Suggested crate layout

```
skilj-agent/
  Cargo.toml            # workspace
  crates/
    index/              # syn-based SkilJ project index (no LLM, fully unit-testable)
    checks/             # testing / layout / performance / error lookup -> Findings
    refactor/           # mechanical changes (context split, new project)
    llm/                # rig wiring: provider config, prompts, extractors
    verify/             # scratch copy + cargo check/test loop
    cli/                # clap commands, the only binary
  references/           # skill text, include_str!'d
  specs/                # this spec
```

`index` and `checks` have no model dependency. They can ship first and are useful right away.
They can also be exposed as an MCP server (rmcp is already in `my-rig-agent`), so Claude Code
could call the same checks.

## Suggested order

1. **`index` + `skilj-agent check`** against skilj-demo and skilj-helpdesk: contexts, types,
   layout and testing findings. No model, so the predictable part is proven first.
2. **Error lookup** from `common-mistakes.md`.
3. **`verify` loop + `write-test`** with Ollama: the first model-driven feature, the one that
   shows whether qwen3:14b is good enough.
4. **`split-context`** on a copy of skilj-helpdesk.
5. **`new`**, then performance.
6. **Issue suggestions** can come early: they're cheap, and every failure from step 3 on is
   worth recording.

## Decisions from elicitation session 1 (2026-10-12)

- Interactive session; the model only maps free text onto a fixed list of tasks.
- Unknown requests: best effort through verified proposals, plus a suggested issue.
- Every failure suggests an issue: unknown request, gave up after retries, a verified proposal
  you discarded, a finding you dismissed as wrong.
- Issues never contain project code; always search for a matching issue first and comment
  there when fairly confident.
- Target model ~14B (qwen3:14b).
- Commands: a `decide()` unit test per branch plus one integration test. Projections: a fold
  unit test per consumed event type plus one integration test.
- Layout style is per project: chosen for new projects, detected then confirmed for existing ones.
- Knowledge lives in skilj-agent; fixes go to skilj only when the gap is in SkilJ itself.

## Decisions from elicitation session 2 (2026-10-12)

- **Debugging** covers build and registration errors, failing tests, unexpected command
  decisions and wrong projections. Event stores in any environment, always read-only. With a
  hosted model, payloads go out only after confirmation, and `sensitive_fields` are always masked.
- **Unexpected decision:** replay `decide()` on the real events. Same outcome as recorded → ask
  whether the code or the expectation is right. Code wrong → shrink and anonymise the events into a
  fixture, then propose a test plus a fix. The proposal is only verified if the test fails without
  the fix and passes with it. Different outcome → the code changed; find the commit that ran and
  replay it.
- **Wrong projection:** check lag first; only refold a caught-up projection.
- **Performance** always starts from measurements (traces when a backend is configured, otherwise a
  local reproduction; lag from the event store; generated load tests) and always ends as advice
  with numbers, never a change proposal. Offer to re-measure once you've followed the advice.
  Load only ever goes to a local database the agent started.
- **SkilJ versions:** older project → offer an upgrade one version step at a time, from
  `CHANGELOG.md`. Newer → suggest updating skilj-agent.
- **Issues:** posted through the forge API when a token is set, otherwise from a pre-filled page in
  the browser. Queued locally while the forge can't be reached.
- **No MCP server:** the deterministic commands also run outside the session with JSON output,
  so Claude Code can use them like `allium check`. The reference markdown can also ship as a skill.
- **Allium plan obligations:** rule and transition categories → unit tests; surface and temporal
  categories → integration tests; type-shape categories are covered by the compiler.
- **Layout:** changing a project's style offers one restructure proposal per context.


## Decisions from follow-up questions (2026-10-12)

- **Repo and issues on GitHub**, reusing an existing `gh` login when there is one. Code may be
  mirrored to Codeberg, but issues stay in one place so the duplicate search sees all of them.
- **skilj-codegen projects are supported:** the index also reads `*.skilj.toml`, and new types
  are declared wherever the context already declares them.
- **Between sessions:** layout style and "not relevant" dismissals are committed in
  `.skilj-agent.toml`; model, forge login, event store connections and queued issues are per user;
  findings are recomputed; proposals and investigations last one session.
- **Unclear requests** are never guessed: the agent asks, offering the candidate tasks.
- **Best effort** gets a fixed tool set: read project files, write only in the scratch copy,
  cargo, read-only event store queries, dependency and latest-skilj lookup, and searching the
  bundled example projects.

## Status (2026-10-12)

`skilj-agent check` is built: step 1 of the suggested order. It indexes a project (contexts,
plugin types, rejection kinds followed through helpers, tests), derives the obligations and
reports untested ones, layout and version. On skilj-helpdesk it finds every rejection kind in
the source and none that aren't there. See the README for usage.

`skilj-agent diagnose` is built: step 2. The error table is generated from the skilj 0.0.10
source (80 error variants, each with its wire code and message); causes and fixes are recorded
for 10 of them, from skilj's `common-mistakes.md`. The other 70 are recognised but have no
recorded fix yet: the first candidates for the knowledge the issues should bring in.


`skilj-agent write-test` is built: step 3. New crates: `verify` (scratch copy of the working
tree, `cargo test`, errors sorted into the proposal's own and the project's), `llm` (rig, Ollama)
and `tasks` (`write_test`). The scratch copy replaces the git worktree from the spec: a worktree
checks out HEAD and would verify against code the developer no longer has.

### First results with qwen3:14b on skilj-helpdesk (2026-10-12)

| Obligation | Result |
|---|---|
| `command.RecordDailyActivity.rejected.already_recorded_today` | verified, attempt 1 (~75 s) |
| `projection.CompanyEngagement.folds.DailyActivityRecorded` | verified, attempt 1 (~65 s) |
| `command.AddInternalNote.rejected.snapshot_unreadable` | verified, attempt 2 |
| `command.AssignTicket.rejected.ticket_not_open` | gave up |
| `command.AutoAssignTicket.accepted` | gave up (before the latest context fixes) |
| `command.ApplyMacro.rejected.requester_mismatch` | gave up (before the latest context fixes) |

Plain `decide()`/`project()` work. Commands deciding from a snapshot (`decide_with`) are the hard
case. What each round taught, all fixed except the last:

- The code holding a rejection kind was missing when it sat in a method of another type
  (`TicketFacts::reject_unless_requester`): context now follows methods of referenced types,
  and always includes the code holding the kind's literal.
- Same-named items from other contexts, binaries and unrelated types' methods crowded the
  context: names now resolve to the type's own file first, then its bounded context.
- The model forgot `use skilj_core::plugin::{Decision, SnapshotState}` (skilj doesn't re-export
  them): added in code now.
- A thinking model can reason without end: answers are capped at 8k tokens and 6 minutes.
- **Open:** the model guesses a snapshot's `NAME` (`"ticket_snapshot"`), so `decide_with` finds
  no snapshot and rejects with `snapshot_unreadable`. Fix: include the `impl Snapshot for X`
  block (its `NAME` and `State`) when a snapshot type is referenced, and say in the task to use
  `X::NAME`. Then rerun the three that gave up.

Also worth considering:
- skilj could re-export `Decision` and `SnapshotState` from `skilj`, like `CommandType`.
- Answers grew to 4–6k tokens once the context grew past ~800 lines: worth measuring
  `--no-think`, and trimming the event payloads a snapshot test doesn't need.
- Issue suggestions are only printed for now; the forge part is still to build.

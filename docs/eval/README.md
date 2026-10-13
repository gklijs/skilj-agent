# write-test evaluations

Baselines from `scripts/eval-write-test.py`, one JSON line per obligation: `outcome`
(`verified`, `gave_up`, `gave_up_maybe_unreachable`, `error`), `attempts`, `seconds`, the last
`problem`, and `no_think`. Compare a new run against these after changing write-test or the model.

| File | Project | Model | Result |
|---|---|---|---|
| `helpdesk-2026-10-13-qwen3-14b.jsonl` | skilj-helpdesk, every untested unit obligation | qwen3:14b, thinking | 154 / 155 verified, 2.8 h |
| `helpdesk-2026-10-13-qwen3-14b-no-think.jsonl` | skilj-helpdesk, 7 chosen obligations | qwen3:14b, `--no-think` | 4 / 7 verified |

Obligation ids name the project's types; no project code is stored. Analysis in
[`../design.md`](../design.md).

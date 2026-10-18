# Temp questions — saved for later (do not block on these)

1. `new`: should it `cargo generate` from `templates/skilj-template` (not in repo yet), or generate a minimal skilj app inline? Which skilj version to pin?
2. `split-context`: for `by_slice`, should each command/projection get its own file with tests, events stay shared — confirm? For `by_kind`, fixed 4 files?
3. `crate_per_context` split: out of scope for now — OK to refuse with a message?
4. skilj-codegen contexts (`*.skilj.toml`): split only the hand-written `.rs`, leave TOML — OK?
5. Performance / runtime-debug / upgrade / interactive session: order after `new`+`split`? Or skip?

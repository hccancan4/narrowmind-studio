# Pre-Phase-4 Mini-Audit

Date: 2026-06-11 · Commit: 35055ae · Scope: three targeted questions ahead of
the LoRA fine-tune phase. Report only — no code changed in this pass. The 1a
and 1c recommendations feed the Phase 4 design prompt; 1b's cheap-hardening
items are scheduled there too.

---

## 1a. `ToolDef` dependency direction

**Question.** `ToolDef` lives in the agent crate (`crates/agent/src/types.rs:78`)
and the orchestrator imports it — conceptually inverted (the orchestrator owns
tools; the agent loop merely consumes their definitions). Phase 4 adds training
tools. Move it to a `crates/protocol` now, or after Phase 4?

**Findings.**
- The dependency is strictly one-way and clean: orchestrator → agent
  (`crates/orchestrator/Cargo.toml`), never the reverse. No circular risk today
  and none introduced by more tools.
- Orchestrator-side imports are already centralized: `tools/registry.rs:21`
  re-exports `ToolDef` from `narrowmind_agent`, and all ~14 tool files import
  through that re-export. The bridge surface is 3 types total (`ToolDef`,
  `ToolDispatcher`, `ToolDispatchOutcome`) in `agent_bridge.rs:11`.
- Churn inventory for a future move: 1 line in `registry.rs` (point the
  re-export at `narrowmind_protocol`), 1 import in `agent_bridge.rs`, agent
  crate re-exports for backward compat, two Cargo.toml entries. Tool files
  themselves don't change. **The cost of moving is constant — it does not grow
  with the Phase 4 tool count**, because new tools also import via the
  re-export.

**Severity:** low. **Recommendation: DEFER to the Phase 5/6 quiet window.**
Moving now spends review bandwidth immediately before the riskiest phase to
buy nothing — coupling does not compound. Keep the discipline of importing
`ToolDef` only via `tools/registry.rs` (already the rule de facto; one line in
AGENTS.md § conventions when we touch it next). **Effort when done: ~0.5 day**
including a `crates/protocol` skeleton, re-export shims, and CI green.

---

## 1b. `run_command` sandbox robustness

**Question.** Is cwd pinning escape-proof (symlinks, UNC paths, relative-path
normalization, env injection)? Phase 4 will run training scripts through this
tool.

**Findings.**
- **cwd pinning is real but is not a containment boundary.**
  `run_command.rs:97` sets `.current_dir(&project.root)` unconditionally; there
  is no `cwd` argument; a test (`cwd_is_pinned_to_project_root`) pins the
  behavior. But pinning only sets the *starting directory*.
- **No validation of `command` or `args`.** `Command::new(&args.command)`
  receives the string as-is (`run_command.rs:95-96`). Absolute paths
  (`C:\Windows\System32\cmd.exe`), UNC paths (`\\server\share\x.exe`), and
  `..`-laden relative args all pass. Symlinks resolve at the OS layer,
  unimpeded. No shell is invoked (argv goes direct — that part is good: no
  quoting/injection surface), and the tool cannot *set* env vars (it inherits
  the app's environment unmodified).
- **Asymmetry with the fs tools.** `read_file`/`write_file`/`list_dir` all
  route through `sandbox::resolve_within` (`sandbox.rs:20-49`), which rejects
  absolute paths, `..` components, and drive prefixes with `ToolError::PathEscape`.
  `run_command` performs none of these checks. The asymmetry is *documented
  intent* — the original design note says an executable allowlist is
  infeasible because the agent must run training scripts, downloads, and
  arbitrary project tooling — but the tool description's "sandbox" phrasing
  oversells what's enforced.

**Severity:** medium — by-design trade-off whose stakes rise in Phase 4
(training scripts) and again in Phase 7 (OSS strangers running prompts they
didn't write).

**Recommendation (three tiers; implement i+ii in Phase 4, decide iii in Phase 7):**
1. **Document the trust model** (ARCHITECTURE.md): `run_command`'s sandbox is
   *spatial by convention, not a security boundary*. It trusts the local user
   and the model acting on their behalf; it exists to keep well-behaved tools
   project-relative, not to contain a hostile one.
2. **Cheap hardening (~2 h):** (a) log the full `command` + `args` at info
   level into `agent.log` — an audit trail for "what did the agent actually
   run"; (b) reject `\\`-prefixed (UNC) commands — no legitimate Phase 4 use,
   and it closes the quietest data-exfil route; (c) align the tool description
   with reality ("working directory is pinned; the executable itself is not
   restricted").
3. **Phase 7 pre-OSS:** evaluate Windows Job Objects (kill-tree + memory caps)
   and a user-facing confirmation for executables other than `uv`/`python`.
   Out of scope before then.

---

## 1c. Training worker process model — **Phase 4 design input**

**Question.** Training is a long batch process (hours — one-shot category) but
needs a live metric stream (loss/lr/grad_norm per step). Does the current
one-shot model support streaming notifications, or do we need a third category
(long-lived-with-streaming)?

**Findings.**
- The one-shot `call_worker` reads **only the first stdout line** as the
  response (`worker.rs:157-168`). If the Python handler wrote a notification
  frame first, parsing fails with `MalformedResponse` and the call dies. As-is,
  one-shot cannot stream.
- The pool's `roundtrip` loop already skips id-less frames
  (`worker_pool.rs:340-396`, explicitly commented as the Phase 4 hook) — but
  the pool's *failure semantics are wrong for training*: in-flight timeout →
  kill, dead child → automatic retry-once. Nobody wants a 3-hour run silently
  re-executed, and a shared serialized child is pointless for a job that owns
  the GPU exclusively.
- The Python server has **no `notify()` helper** — it only writes one response
  per request (`rpc/server.py:93`). The protocol slot exists (id-less frames
  are valid JSON-RPC notifications and the server already *accepts* them
  inbound), it's just never been used outbound.
- `ToolEvent::Progress` already flows worker→UI (synth_gen precedent,
  `synth_gen/mod.rs:541`), and `run_command` demonstrates live line streaming
  (`forward_lines`, `run_command.rs:164-189`) — but raw stdout lines lose the
  structure the Training Monitor needs.

**Recommendation: no third process-model category. Extend one-shot with
notification support — "one-shot-with-streaming" — plus filesystem-as-truth
for metrics.** Concretely (Phase 4, ~0.5-1 day):

1. **Python `notify(method, params)` helper** in `rpc/server.py` (~15 LOC):
   writes an id-less JSON-RPC frame to the protocol stdout (the saved
   `proto_stdout`, NOT the redirected `sys.stdout`). Handlers call it freely
   mid-execution.
2. **Rust `call_worker_with(runner, cmd, on_notify)`**: same one-shot lifecycle
   (spawn → request → response → exit; kill = clean cancel; **no retry**), but
   the read phase becomes the pool's id-matching loop — id-less frames go to
   the `on_notify` callback instead of being skipped. Existing `call_worker`
   delegates with a no-op callback; zero behavior change for current callers.
3. **Activity-based deadline**: each notification refreshes the timeout. A
   3-hour training run with steady step events needs no absurd static ceiling;
   the meaningful hang signal is "N minutes of *silence*" (suggest N=10 min,
   in the timeout registry with rationale).
4. **Metrics are dual-written; the file is the source of truth.** The training
   worker emits `{"method":"training.metric","params":{step,loss,lr,grad_norm,…}}`
   notifications for the live UI (→ `ToolEvent::Progress` or a new typed
   `ToolEvent::Metric`), **and appends the same record to
   `runs/<run_id>/metrics.jsonl`**. The notification stream is an optimization;
   the file is durable. This is what makes ROADMAP's "resume / re-attach after
   app restart" deliverable cheap: re-attach = check `runs/<run_id>/worker.pid`
   liveness + tail `metrics.jsonl`. No stdio re-binding, no broken-pipe
   complexity, full alignment with AGENTS principle 3 (filesystem is the
   source of truth) and principle 7 (streaming over polling — for the live
   path) simultaneously.

**Why not the pool:** training runs own the GPU exclusively and run for hours —
there is nothing to amortize (the pool exists to amortize model-load cost
across many small calls) and both of the pool's recovery behaviors
(timeout-kill, retry-once) are actively harmful for a long stateful job.
**Why not run_command:** it works mechanically (live stdout lines), but the
Training Monitor needs structured, typed metrics; parsing log lines back out
of `ToolEvent::Stdout` is a regex contract that breaks the first time Unsloth
changes its progress format. JSON-RPC notifications keep the schema explicit
end to end.

---

## Summary table

| # | Question | Severity | Action | When | Effort |
|---|----------|----------|--------|------|--------|
| 1a | ToolDef in agent crate | low | defer move to `crates/protocol`; keep importing via registry.rs re-export | Phase 5/6 window | 0.5 d |
| 1b | run_command sandbox | medium | document trust model + audit-log + UNC reject + honest description | Phase 4 (docs+hardening) / Phase 7 (containment) | 2 h now, larger later |
| 1c | training worker model | — (design) | one-shot-with-streaming: `notify()` + `call_worker_with` + activity deadline + `metrics.jsonl` as truth | Phase 4 M1 | 0.5-1 d |

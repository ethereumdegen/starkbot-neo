# 15 — The heartbeat

Decisions: [P13](00-decisions.md) (the file and the timer), [A26](00-decisions.md)
(declared CLIs, the only command execution), [A27](00-decisions.md) (no overlap,
no catch-up). Constrained by P3 (not a shell/coding agent), P5/A8 (gates),
P2′ (GTM purpose).

## 1. What it is

A file the user writes, that Starkbot acts on while the user is not there.

```
~/Library/Application Support/com.starkbot.neo/heartbeat.md
```

```markdown
# Every hour

Using the octaweave cli, read the active tasks on the Q4 Launch kanban board
and do them. Skip anything that needs a spend approval — I'll do those myself.

Then check the campaign sheet in Numbers and fill in yesterday's numbers from
the analytics tab.
```

That is the whole format: **Markdown prose, addressed to Starkbot**. There is no
front matter to learn, no YAML, no step syntax, no DSL. The file's content
becomes the **goal** of one task, so a tick is indistinguishable from the user
having typed the same paragraph into the Conversation pane — same routes, same
caps, same gates, same trace.

Why a file and not a settings field: the user edits it in any editor, keeps it
in version control if they like, and can write a page of standing instructions
without fighting a text box. Why Markdown and not a schedule format: the
scheduling lives in settings, where it can be validated; the *work* is a
sentence, which is the only interface the agent has ever needed.

### What it is not

- Not a cron expression language. One cadence, one file. A user who needs
  "weekdays at 9" writes that as prose and the tick that runs at 03:00 decides
  it has nothing to do — or, for real precision, uses the OS scheduler to run
  `neo heartbeat run` and turns the timer off.
- Not a second agent. There is no heartbeat-specific model, prompt or tool set.
- Not a way around P3. See §4.

## 2. Settings

`heartbeat.*` in `neo-core::settings`, a new `HeartbeatSettings` section beside
`QueueSettings`:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `enabled` | `bool` | `false` | Off until the user turns it on. A harness that starts acting on its own the first time it launches is a surprise, and a surprise that spends money. |
| `every` | duration | `1h` | Cadence. Minimum `5m`, maximum `7d`; validated, so `0` is rejected rather than spinning. |
| `file` | path | `heartbeat.md` (relative to the config home) | Where the instructions live. Absolute paths allowed so the file can sit in a synced folder. |
| `max_task_duration` | duration | `30m` | A tick that runs longer is cancelled and recorded as timed out, so one wedged run cannot hold the single slot forever. |
| `caps` | `CapSettings` override | inherit | Per-tick spend/step caps. An unattended run may be given a tighter budget than an attended one. |
| `quiet_hours` | optional `HH:MM–HH:MM` | none | A window in which ticks are skipped (recorded as skipped). For users who do not want their machine driving apps at 3 a.m. |
| `on_gate` | `hold` \| `skip` | `hold` | What a tick does when a task needs confirmation. `hold` parks the task for the user; `skip` abandons the item and records it. **There is no `approve`.** A timer is not consent (A26). |

`neo settings` gets `heartbeat` rows; the TUI Settings pane edits them; the
Doctor tab renders the heartbeat row of §6.

## 3. Scheduler semantics

Owned by the core, not a front end — the heartbeat must run with the TUI closed
and the desktop app quit, so it lives in `neo-agent` behind the `Runtime` and is
driven by whichever process holds the store.

- **One at a time.** A single slot per install. A tick due while one is running
  is **dropped**, not queued (A27): an hour-long stall must not produce twelve
  pending copies. The drop is recorded.
- **No catch-up.** Sleep, shutdown and quit are not replayed. `last_tick_at` is
  advanced to the boundary, and the next tick happens on the next boundary.
  Replaying eight missed hours of "do the active tasks" would do the same work
  eight times.
- **Backoff.** Consecutive failures multiply the interval by 2, capped at 6 h,
  and reset on the first success. A broken `octaweave` binary or a revoked
  token should not be retried 24 times a day.
- **Skip is a real outcome.** Missing file, empty file, quiet hours, disabled:
  each is recorded as a skipped tick with its reason, so the Doctor row can say
  *why* nothing has happened, which is the question a user actually asks.

Every tick is persisted as a `heartbeat_tick` row — `started_at`, `finished_at`,
`outcome` (`done` | `held` | `failed` | `timed_out` | `skipped(reason)` |
`dropped`), the resulting task id, and the byte length of the goal (never a
copy of it; the file is the user's, and it is already on disk).

## 4. Calling a CLI without becoming a shell

The user's example — *"using octaweave cli"* — needs command execution, and P3
forbids a shell/coding agent. Both hold, because what A26 adds is not a shell.

**Declared, in settings, by the user:**

```toml
[[tools.cli]]
name    = "octaweave"              # what the model sees
program = "/opt/homebrew/bin/octaweave"   # absolute path, resolved once
args    = ["--json"]               # fixed leading argv, always sent
mutates = true                     # gated like any outward action
cwd     = "~/work/q4-launch"       # optional, pinned
```

**What the model may choose:** which declared entry, and the trailing arguments.
Nothing else.

**What that forbids, by construction:** the argv vector goes to
`std::process::Command` with that exact `program` — there is no shell, so `;`,
`&&`, `|`, `$(…)`, `>`, `~` and `*` are literal characters with no meaning. The
model cannot name a different binary, cannot reach `/bin/sh`, cannot set the
environment, and cannot change the working directory. A program not in the list
does not exist. Arguments are additionally rejected if they contain a NUL byte
or exceed a length cap.

**Gating.** `mutates = true` makes every call a confirm card quoting the exact
argv (P5/A8). During a heartbeat tick, `on_gate` decides: `hold` parks it for
the user, `skip` drops the item. Never auto-approve — the whole point of a gate
is that a human agreed, and a clock is not a human.

**Output.** stdout and stderr are captured, truncated to a byte cap
(16 KiB each), and attached to the trace. The exit status is reported to the
model. A CLI that wants to be useful here should speak JSON; nothing requires
it to.

**Secrets.** A declared CLI inherits a *scrubbed* environment: `PATH`, `HOME`,
`LANG` and nothing else, unless the user lists specific variable names to pass
through. Starkbot's own keys are never in that environment, and no key is ever
an argument.

## 5. Surfaces

```
neo heartbeat status              # enabled, cadence, last tick, next due, outcome
neo heartbeat edit               # opens settings.heartbeat.file in $EDITOR
neo heartbeat run                # run one tick now, in the foreground, printing the trace
neo heartbeat enable | disable
```

`neo heartbeat run` is also the hook for the OS scheduler, for users who want
real cron semantics: turn `enabled` off and let `launchd` call it.

In the TUI ([14](14-tui.md)): a heartbeat row in the Doctor pane, the tick
history in the Queue pane (ticks are tasks, so they are already there), and `h`
to run one now.

## 6. Doctor row

```
heartbeat        ok    every 1h · last 14:02 done · next 15:02
heartbeat        warn  enabled but heartbeat.md is empty
                       fix: `neo heartbeat edit`
heartbeat        warn  3 consecutive failures · backed off to 8h · next 21:14
heartbeat        —     disabled
```

## 7. Order of work

1. `HeartbeatSettings` + validation + `neo settings` rows. (Settings are the
   contract the rest hangs off.)
2. The file: read, trim, empty/missing handling, `neo heartbeat edit`.
3. `neo heartbeat run` — one tick in the foreground, as a normal task. This is
   the acceptance surface: if the goal in the file works when run by hand, the
   timer adds nothing but timing.
4. The timer: single slot, no catch-up, backoff, quiet hours, tick rows.
5. `tools.cli[]` declarations + the argv tool + gating + trace capture.
6. Doctor row and TUI wiring.

Acceptance for the user's own example, end to end: with `octaweave` declared and
a `heartbeat.md` that says *read the active tasks on the Q4 Launch board and do
them*, `neo heartbeat run` reads the board through the declared CLI, works the
items through the ordinary routes (browser, apps, spreadsheets), gates every
mutating call, and leaves one trace showing what it did.

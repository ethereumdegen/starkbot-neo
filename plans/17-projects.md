# 17 — Projects, and the heartbeat per project

Supersedes the single-file scheduler in [15-heartbeat](15-heartbeat.md): the
heartbeat is **per project**, not per install. Everything else in 15 — the
prose format, the tick semantics, the declared-CLI rule, what a gate does to
an unattended run — still holds and is not repeated here.

## 1. What a project is

A named piece of standing work with two files and a clock:

```
<root>/soul.md        who Starkbot is on this project, and what it must know
<root>/heartbeat.md   what to do, every tick, addressed to Starkbot in prose
```

Nothing else. No manifest, no YAML, no task list format. A project is a name,
a folder, an interval and those two files — which means a user can create one
in ten seconds and read the whole of it at a glance.

**Where the folder is.** Either the user names one (a repo, a synced folder —
their editor, their version control), or they name nothing and Starkbot keeps
it under `<data_dir>/projects/<slug>/`. The managed case is the default,
because "make me a project called Q4 Launch" should not open a file picker.

**What Starkbot may read and write there.** Exactly `soul.md` and
`heartbeat.md`, by name. Not the folder, not a glob, not a subdirectory. P3
is intact: this is not file access, it is two named documents the user
chose to share, the same way a conversation is shared. A project root that
does not exist, or that is not a directory, is an error the user sees — never
a path Starkbot creates outside its own data dir.

## 2. The two files

**`heartbeat.md`** is the goal of one task, verbatim, exactly as 15 §1
describes: Markdown prose addressed to Starkbot. A tick is indistinguishable
from the user pasting the same paragraph into the composer — same routes,
same caps, same gates, same trace. Empty or missing means the tick is
*skipped with a reason*, not failed.

**`soul.md`** is the project's standing context, in P8's sense: voice, facts
about this work, preferences. It is prepended to the turn's preamble for
every task run in the project — heartbeat ticks *and* conversations the user
starts in it. **Preferences, not permissions** (P8): nothing in `soul.md` can
loosen a safety rule, raise a cap, or pre-approve a confirm. It is read fresh
on each turn, so editing it takes effect on the next one with nothing to
reload.

There is one install-wide `soul.md` already (P8). A project's own file does
not replace it: the install's identity comes first, the project's context
after, and where they disagree about *style* the project wins — where they
disagree about *policy*, neither wins, because neither may set policy.

## 3. The clock

Per project, in the project row rather than in settings — two projects want
two cadences, which is the whole reason this moved off the install:

| Field | Default | Meaning |
|---|---|---|
| `heartbeat_enabled` | `false` | Off until the user turns it on. A harness that starts acting on its own is a surprise, and a surprise that spends money (15 §2). |
| `heartbeat_every` | **4 h** | Cadence. Minimum 5 m, maximum 7 d, validated. |
| `last_tick_at` / `next_due_at` | — | Advanced on every tick, including a skipped one. |

Semantics are 15 §3 unchanged, now scoped per project: **one tick at a time
across the whole install** (a tick due while any tick runs is dropped and
recorded — two projects driving one keyboard is the same problem as two
Starkbots), **no catch-up**, **backoff** ×2 on consecutive failures capped at
6 h, and **skip is a real outcome** with a reason. Every tick is a
`heartbeat_ticks` row: project, started, finished, outcome, task id, and the
byte length of the goal — never a copy of it.

`on_gate` stays `hold` (park it for the user) or `skip`. There is no
`approve`: a clock is not consent (A26).

## 4. Surfaces

```sh
neo projects                       # the index
neo projects add "Q4 Launch" [--root ~/work/q4]
neo projects show q4-launch        # files, clock, recent ticks
neo projects edit q4-launch --heartbeat | --soul     # $EDITOR
neo projects heartbeat q4-launch --every 4h --on|--off
neo heartbeat run q4-launch        # one tick now, in the foreground
```

**TUI** ([14](14-tui.md)): a Projects page listing every project with its
cadence and last tick, and a project page showing both files, the clock, and
the recent ticks — `e` edits a file, `r` runs a tick now, `t` toggles the
timer. It is a page in the existing one-page-at-a-time layout, not a new
pane concept.

**Desktop** ([04](04-ui.md)): the same two screens — an index and a show page
— over the same events. No new bridge concepts: projects are rows and events
like conversations are.

## 5. Order of work

1. `Project` in `neo-core` + the `projects` and `heartbeat_ticks` tables.
2. The file pair: read, write, create-on-demand in the managed root, empty
   handling.
3. `neo heartbeat run <project>` — one tick as an ordinary task, with
   `soul.md` in the preamble. **This is the acceptance surface**: if the goal
   in the file works when run by hand, the timer adds nothing but timing.
4. The scheduler: one slot, no catch-up, backoff, tick rows.
5. TUI pages, then desktop pages.

Acceptance: a project created with a two-line `heartbeat.md` runs by hand and
then on its own four hours later; its `soul.md` demonstrably reaches the
turn (a fact stated only there comes back in the answer); a tick that trips a
confirm parks it for the user instead of approving it; both front ends list
the project and show its last tick.

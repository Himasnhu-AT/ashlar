# Improvement plan

A competitive audit of ashlar (performance, features, tooling) and the work that came out of it.
These files are the single source of truth for that work: read them before starting, update them
in the same commit as the change.

| File | What it holds | Who updates it |
|---|---|---|
| [AUDIT.md](AUDIT.md) | What the audit found: measured numbers, gaps, bugs, with file:line | Once, then only corrections |
| [ROADMAP.md](ROADMAP.md) | Phases and milestones, in order, with their exit criteria | When a phase starts or ends |
| [TASKS.md](TASKS.md) | Every task: id, why, files, acceptance criteria, how to measure | When a task is added or its scope changes |
| [TODO.md](TODO.md) | The checklist: status and owner of every task | Every time a task changes state |
| [PROGRESS.md](PROGRESS.md) | Dated log: what landed, numbers before and after, decisions, surprises | After every merged task |

## Rules

- **Task ids** are `IMP-<phase>.<n>` (`IMP-1.3`). One task id per commit, as AGENTS.md §4 asks
  (`IMP-1.3: cache text layout per revision`).
- **Nothing is done until it is measured or tested.** A performance task records its numbers before
  and after in PROGRESS.md (`cargo xtask bench`, or the perf test named in TASKS.md). A feature task
  lands with tests.
- **Don't assume, check.** When a task depends on a fact about the code, confirm it at the file:line
  first; when a fact in these files turns out wrong, fix the file.
- **No other framework is named** anywhere in the repository, commits or PRs: write "the reference
  framework" or describe the technique. Check the staged diff and the message before every commit.
- **Parallel work** follows AGENTS.md §6: one worktree per agent under `.claude/worktrees/<name>`,
  branch `wt/<name>`, own `CARGO_TARGET_DIR`; the integrator merges into `imp`, runs the full gate,
  and updates TODO.md and PROGRESS.md.
- Heavy commands run through `memgate` (`memgate -- cargo test -p ashlar-ui`).

## Status values (TODO.md)

`todo` · `doing (<owner>)` · `review` · `done (<commit>)` · `blocked (<why>)` · `dropped (<why>)`

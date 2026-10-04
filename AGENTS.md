# AGENTS.md

marquee — a terminal marquee/scrolling-text CLI in Rust, focused on correct
CJK/emoji/mixed-width Unicode scrolling measured in terminal display columns.

## Commands

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo run -- "你好世界 · Hello Terminal 🚀"   # manual smoke
```

All three gates must pass before any task card goes to Review. No unnecessary
`unsafe`.

## Work board

Work is tracked in the vuv vault `bip` (build-in-public), project **P-1
(marquee)**. Follow the vuv work loop: `next_ready` → `claim` → Doing → work →
tick checklist → Review. Agents never move tasks to Done; a human accepts.
Design docs live in `docs/`.

## Git conventions (w-git-branch-hygiene)

Hierarchy — depth never exceeds 2:

```
main                 # product; always runnable; no normal development here
├── feat/*           # one coherent outcome → one final PR
│   └── task/*       # temporary parallel subwork; merges into its feat/*
└── fix/*            # independent bug fix → PR
```

- Name branches by purpose, never by executor (`feat/marquee-core`, not
  `claude-task`).
- Work directly on the `feat/*` branch unless work is genuinely parallel;
  create `task/*` only then, and delete it after local merge into `feat/*`.
- Delete branches after integration; keep worktrees 1:1 with branches.
- Commits: Conventional Commits, referencing the vuv task id, e.g.
  `feat(unicode): precompute grapheme cells with display widths (T-2)`.

### Current branch map

| Branch               | Scope                                        | vuv tasks   |
|----------------------|----------------------------------------------|-------------|
| `feat/marquee-core`  | v1 core CLI (unicode, cli, terminal, engine, renderer, run loop, README) | T-2..T-8 |
| `feat/big-font-mode` | big-font mode design + implementation (docs/big-font-mode.md)            | T-9..T-12 |
| `feat/themes-and-continuous-scroll` | theme layer (`src/theme.rs`, `--theme`/`--theme-file`) and continuous scrolling (`--continuous`); vuv cards not yet created | — |

### Integration

Remote: `origin = git@github-yaoyao-hunter:yaoyao-hunter/marquee.git` (GitHub
repo may not exist yet — pushes fail with "Repository not found" until it is
created). While there is no remote, `feat/* → main` integration is a local
merge performed **after** the human accepts the corresponding vuv task(s);
once the remote exists, switch to real PRs. Never `task/* → main` directly.

## Identity

Repo-local git identity: `yaoyao-hunter <32746788+yaoyao-hunter@users.noreply.github.com>`. Push identity:
SSH alias `github-yaoyao-hunter` (key `~/.ssh/id_ed25519_yaoyao_hunter`).
Do not push under any other alias without being asked.

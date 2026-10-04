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

All three gates must pass before any change goes to review. No unnecessary
`unsafe`.

## Workflow

- `main` stays runnable; no normal development happens directly on it.
- Work on a `feat/*` branch (one coherent outcome → one final PR) or a `fix/*`
  branch (an independent bug fix → PR). Parallel subwork may use short-lived
  `task/*` branches that merge into their `feat/*` and are then deleted.
- Name branches by purpose, never by executor (`feat/marquee-core`, not
  `claude-task`).
- Integration: `feat/* → main` through pull requests on GitHub, merged by the
  maintainer. Delete the branch (local and remote) after the merge. Never
  open `task/* → main` PRs.
- Push to `origin` exactly as configured; do not change remotes or push
  under any other identity.
- Commits follow Conventional Commits, e.g.
  `feat(unicode): precompute grapheme cells with display widths`.

## Docs

- `docs/marquee.1` — the manual page (flags, exit codes, environment)
- `docs/manual.md` + `docs/manual.zh-CN.md` — the big-font mode manual
- `docs/themes.md` — the theme catalogue
- `docs/big-font-mode.md` — the big-font design doc
- Screenshots are GIFs in `docs/images/`, recorded with
  [vhs](https://github.com/charmbracelet/vhs) from the tapes in `tools/vhs/`:
  `cd docs/images && vhs ../../tools/vhs/<name>.tape`

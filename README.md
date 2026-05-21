# Claude.md Toggler

Cross-platform menu bar app to switch between Claude Code harness profiles with one click.

Toggle the global `~/.claude/CLAUDE.md` between pre-defined or custom profiles (`token-save` / `speed-first` / `quality-first` / `unlimited` / custom). The original is preserved in `CLAUDE.md.origin`; toggling is an atomic file swap.

## Why

`~/.claude/CLAUDE.md` is your global Claude Code harness. The right harness depends on the moment — sometimes you want terse token-saving output, sometimes quality-first TDD discipline, sometimes deep analysis with no budget. Editing the file by hand each time loses the original and is slow. This app turns it into a one-click toggle.

## File convention

All profile files live next to `~/.claude/CLAUDE.md`, sharing the `CLAUDE.md.{suffix}` prefix:

```
~/.claude/
├── CLAUDE.md                  ← active (what Claude Code reads)
├── CLAUDE.md.origin           ← backup of the default state
├── CLAUDE.md.token-save       ← pre-defined profile
├── CLAUDE.md.speed-first      ← pre-defined profile
├── CLAUDE.md.quality-first    ← pre-defined profile
├── CLAUDE.md.unlimited        ← pre-defined profile
└── CLAUDE.md.{custom-name}    ← user-defined profiles
```

Because the suffix is not `.md`, Claude Code's exact-match auto-load ignores them — they're inert until you toggle one in.

## Stack

- **Tauri 2.x** (Rust core + webview frontend)
- **Frontend**: React + TypeScript + Vite
- **Backend**: Rust (`std::fs`, `notify`, `similar`, `rusqlite`)
- **OS**: macOS / Windows / Linux

## Dev

```bash
pnpm install
pnpm tauri dev
```

## Status

v0.1 in progress — see `docs/` (planned) and the spec at [msa/ideabank/docs/16-claude-md-toggler.md](https://github.com/1989v/msa-public-or-similar) for the full feature scope.

## License

MIT

# siki

A TUI orchestrator for managing multiple [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across git worktrees.

```
┌─ Projects ────┬─ Claude Code ───────┬─ Source Tree ───────┐
│ ▾ my-app      │ ○ claude  □ app.rs  │ ▸ src/              │
│   ├ ○ tokyo   │                     │ ▸ tests/            │
│   └ ● osaka   │ Working on feature  │   Cargo.toml        │
│ ▾ api         │ auth flow...        │   README.md         │
│   └ ○ kyoto   │                     ├─ Terminal ──────────┤
│               │ [Tool: Edit]        │ $ cargo test        │
│               │                     │ running 245 tests   │
│               │                     │ test result: ok     │
├───────────────┴─────────────────────┴─────────────────────┤
│ worktree 追加完了: osaka (feature/auth)                    │
└───────────────────────────────────────────────────────────┘
```

## Features

- **Multi-worktree management** - Create, select, and archive git worktrees from a single interface
- **Integrated Claude Code** - Each worktree gets its own Claude Code terminal session
- **Built-in terminal** - Up to 5 terminal tabs per worktree, no need to leave the TUI
- **Source tree & diff viewer** - Browse files and view git diffs in the right panel
- **File viewer with syntax highlighting** - Open and navigate source files with search
- **Project-wide grep** - Search across your entire worktree
- **siki.json scripts** - Define `setup`, `run`, and `archive` hooks per project
- **Auto-discovery** - Projects in `~/.siki/workspaces/` are detected automatically

## Installation

```bash
cargo install --git https://github.com/drapon/siki
```

Or via Homebrew:

```bash
brew tap drapon/tap
brew install siki
```

### Prerequisites

- [Rust](https://rustup.rs/) (for `cargo install`)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI installed and authenticated
- Git

## Quick Start

1. Run `siki`
2. Press `A` to add a project (enter the path to a git repository)
3. Press `a` on a project to create a worktree (enter a branch name)
4. Press `Enter` on a worktree to open it - Claude Code and a terminal start automatically

## Key Bindings

### Global

| Key | Action |
|-----|--------|
| `q` | Quit |
| `?` / `F1` | Help |
| `Tab` | Next panel |
| `Shift+Tab` | Previous panel |

### Left Panel (Projects)

| Key | Action |
|-----|--------|
| `j` / `k` | Move cursor |
| `Space` | Collapse / expand project |
| `Enter` | Select worktree |
| `a` | Add worktree |
| `A` | Add project |
| `r` | Run script (`siki.json`) |
| `d` | Archive worktree |

### Main Panel

| Key | Action |
|-----|--------|
| `Tab` | Next tab |
| `w` | Close tab |
| `i` | Open new Claude Code tab |
| `Ctrl+\` | Detach from Claude tab |
| `/` | Search in file |
| `g` | Grep search |
| `s` | Send file:line to Claude |

### Right Panel

| Key | Action |
|-----|--------|
| `t` | Toggle Tree / Diff view |
| `j` / `k` | Navigate |
| `h` / `l` | Collapse / expand |
| `Enter` | Open file |

### Terminal

| Key | Action |
|-----|--------|
| `n` | New terminal |
| `Ctrl+n` | New tab |
| `Ctrl+1-5` | Switch tab |
| `Ctrl+\` | Detach |

## Configuration

### `~/.siki/config.toml`

```toml
[siki]
shell = "/bin/zsh"           # Shell for terminal sessions
shared_dirs = ["node_modules", ".next"]  # Symlinked into worktrees
```

### `siki.json` (in project root)

```json
{
  "scripts": {
    "setup": "npm install",
    "run": "npm run dev",
    "archive": "echo cleanup"
  }
}
```

- **setup** - Runs automatically when a worktree is created
- **run** - Triggered with `r` key on a worktree
- **archive** - Runs before a worktree is removed with `d` key

## Directory Structure

```
~/.siki/
├── config.toml              # Global configuration
└── workspaces/              # All worktrees live here
    ├── my-app/
    │   ├── project.json     # Project metadata (source repo path)
    │   ├── tokyo/           # Worktree (git worktree)
    │   └── osaka/           # Worktree (git worktree)
    └── api/
        └── kyoto/
```

## License

[MIT](LICENSE)

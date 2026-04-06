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
- **Integrated Claude Code** - Each worktree gets its own Claude Code terminal session with multiple tabs
- **Inter-session messaging** - Send messages, broadcast, and hand off context between Claude Code sessions via MCP
- **Session state monitoring** - Track session states (idle/working/waiting) via Unix socket broker and Claude Code hooks
- **Built-in terminal** - Up to 5 terminal tabs per worktree, no need to leave the TUI
- **Text selection & clipboard** - Select and copy text in both Claude and terminal panes
- **Scrollback** - 5000-line scrollback buffer for Claude tabs with per-tab scroll state
- **Source tree & diff viewer** - Browse files and view git diffs in the right panel
- **File viewer with syntax highlighting** - Open and navigate source files with search
- **Project-wide grep** - Search across your entire worktree
- **siki.json scripts** - Define `setup`, `run`, and `archive` hooks per project
- **Auto-discovery** - Projects in `~/.siki/workspaces/` are detected automatically
- **Project filtering** - Filter by `-p` flag or auto-detect from current directory

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

## CLI Usage

```
siki [OPTIONS] [COMMAND]
```

### Commands

| Command | Description |
|---------|-------------|
| `siki` | Launch the TUI |
| `siki list` | List all projects and worktrees |
| `siki mcp` | Start MCP stdio server (for Claude Code integration) |

### Options

| Flag | Description |
|------|-------------|
| `-p <name>` | Filter projects by name prefix |
| `-h, --help` | Print help |
| `-V, --version` | Show version |

## Key Bindings

### Global

| Key | Action |
|-----|--------|
| `q` | Quit |
| `?` / `F1` | Help (scroll: `j`/`k`) |
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
| `d` | Archive worktree / remove project |
| `S` | Create `siki.json` for project |

### Main Panel (File Viewer)

| Key | Action |
|-----|--------|
| `j` / `k` | Scroll up / down |
| `Tab` | Next file tab |
| `w` | Close tab |
| `i` | Open new Claude Code tab |
| `Ctrl+\` | Detach from Claude tab |
| `/` | Search in file |
| `n` / `N` | Next / previous match |
| `g` | Grep search |
| `s` | Send file:line to Claude |
| `Shift+Up/Down` | Scroll Claude pane (scrollback) |
| `Shift+PageUp/PageDown` | Scroll Claude pane 10 lines |

### Right Panel (Source Tree / Diff)

| Key | Action |
|-----|--------|
| `t` | Toggle Tree / Diff view |
| `j` / `k` | Navigate (Tree) / Scroll (Diff) |
| `h` / `l` | Collapse / expand (Tree) |
| `Enter` | Open file (Tree) |
| `/` | Search in tree |

### Claude Terminal

| Key | Action |
|-----|--------|
| `Ctrl+t` | New Claude tab |
| `Ctrl+w` | Close Claude tab |
| `Ctrl+r` | Resume previous session |
| `Ctrl+\` | Detach (return to main) |
| `Tab` / `Shift+Tab` | Next / previous tab |
| `Shift+Up/Down` | Scroll (scrollback) |
| `Shift+PageUp/PageDown` | Scroll 10 lines |

### Terminal

| Key | Action |
|-----|--------|
| `n` | New terminal |
| `Ctrl+n` | New tab |
| `Ctrl+1-5` | Switch tab |
| `Ctrl+\` | Detach |

## MCP Server

siki includes a built-in MCP server that enables Claude Code sessions to communicate with each other. It starts automatically via `siki mcp` and is configured in each worktree's `.mcp.json`.

### Available Tools

| Tool | Description |
|------|-------------|
| `list_sessions` | List all active sessions with summaries and pending messages |
| `send_message` | Send a message to a specific session, worktree, or project |
| `broadcast` | Broadcast a message to all sessions (or scoped to project/worktree) |
| `set_summary` | Update the current session's work summary |
| `handoff` | Hand off context to another session (includes git state) |
| `get_context` | Fetch context from another session or worktree |

## Configuration

### `~/.siki/config.toml`

```toml
[siki]
shell = "/bin/zsh"                          # Shell for terminal sessions
shared_dirs = ["node_modules", ".next"]     # Symlinked into worktrees
base_branch = "origin/main"                 # Default base branch
```

### `siki.json` (in project root)

```json
{
  "scripts": {
    "setup": "npm install",
    "run": "npm run dev",
    "archive": "echo cleanup"
  },
  "base_branch": "origin/develop"
}
```

| Field | Description |
|-------|-------------|
| `scripts.setup` | Runs automatically when a worktree is created |
| `scripts.run` | Triggered with `r` key on a worktree |
| `scripts.archive` | Runs before a worktree is removed with `d` key |
| `base_branch` | Project-specific base branch (overrides config.toml) |

## Directory Structure

```
~/.siki/
├── config.toml              # Global configuration
├── siki.db                  # SQLite database (sessions, messages)
├── sock                     # Unix socket for session broker
└── workspaces/              # All worktrees live here
    ├── my-app/
    │   ├── project.json     # Project metadata (source repo path)
    │   ├── tokyo/           # Worktree (git worktree)
    │   │   ├── .mcp.json    # MCP server config (auto-generated)
    │   │   └── .claude/     # Claude Code session data
    │   └── osaka/
    └── api/
        └── kyoto/
```

## License

[MIT](LICENSE)

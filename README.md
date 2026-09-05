# 👻 ghostdu

> **Modern, blazingly fast, native Linux disk usage & ghost file analyzer with wastebin support and zero-flag headaches.**

`ghostdu` is an ultra-fast, compact (1.3 MB native binary) terminal disk usage analyzer engineered in Rust. Inspired by `ncdu`, it is designed to eliminate common frustrations: no need to remember complex exclusion flags, automatic isolation of Docker and ghost artifacts, and native FreeDesktop wastebin/trash integration alongside permanent removal.

---

## ⚡ Key Highlights

- **Zero-Flag Intelligence**:
  - Automatically skips Linux virtual and pseudo filesystems (`/proc`, `/sys`, `/dev`, `/run`, `efivarfs`, `tmpfs`) without needing `--exclude` or `-x`.
  - Inode deduplication prevents hardlinks (common in Docker layers and Git repos) from artificially inflating disk usage calculations.
  - Measures both true allocated disk blocks (`st_blocks * 512`) and apparent file sizes. Toggle with `A`.

- **Automated Docker & Ghost File Separation**:
  - **Docker Engine Direct Inspection**: Queries `/var/run/docker.sock` to report active vs reclaimable images, stopped containers, unattached volumes, and BuildKit caches.
  - **Open Unlinked Ghost File Discovery**: Scans `/proc/*/fd` to expose deleted files that are still held open by active processes and silently consuming disk space.
  - **Live Filter Toggles (`G`)**: In the explorer tree, press `G` to cycle between *Show All*, *Hide Ghost Files*, or *Ghost Files ONLY*—no command line flags required!
  - **Dedicated Ghost Inspector (`Tab` / `g`)**: Dedicated panel showing Docker reclaimable storage breakdown and open deleted files with one-touch pruning (`p`).

- **Wastebin & Permanent Deletion**:
  - **Move to Wastebin (`t` / `w`)**: FreeDesktop.org Trash specification compliant (`~/.local/share/Trash`). Safe, recoverable, and visible in Dolphin, Nautilus, Thunar, or `trash-cli`.
  - **Completely Remove (`d` / `D`)**: Permanent deletion with clear red safety confirmation modals, size preview, and batch processing.
  - **Multi-Selection (`Space`)**: Mark multiple files and directories across folders, with running counts and sizes.

- **Lightweight & High Performance**:
  - 1.3 MB standalone native binary (stripped, LTO enabled, zero runtime overhead).
  - Parallel background scanning via Rayon with real-time animated progress.
  - Both interactive Ratatui TUI and auto-detected non-interactive summary mode (`ghostdu --summary` or piped output).

---

## ⌨️ Keyboard Shortcuts

| Key | Action |
|:---|:---|
| **Navigation** | |
| `j` / `Down` | Move cursor down |
| `k` / `Up` | Move cursor up |
| `Enter` / `l` / `Right` | Enter / drill down into directory |
| `Backspace` / `h` / `Left` | Go up to parent directory |
| `Home` / `End` | Jump to top / bottom |
| **Selection & Deletion** | |
| `Space` | Toggle selection on current item |
| `a` | Select all / unselect all visible items |
| `t` / `w` | **Move selected (or current) to Wastebin / Trash** |
| `d` / `D` | **Completely Remove (Permanent Delete)** |
| **Ghost & Docker Management** | |
| `Tab` / `g` | **Switch between Explorer & Ghost/Docker Inspector** |
| `G` | **Cycle Ghost filter (`All` → `Hide Ghost` → `Ghost ONLY`)** |
| `p` | **Prune Docker dangling resources** (in Ghost Inspector) |
| `1` / `2` | Switch tabs in Ghost Inspector (Docker vs Deleted Files) |
| **Search & Display** | |
| `/` | Interactive live search/filter (type to match, `Esc` to clear) |
| `s` | Cycle sort order (*Size desc*, *Size asc*, *Name*, *Item count*) |
| `A` | Toggle Apparent size vs Actual block disk usage |
| `r` | Rescan directory / refresh Docker and ghost stats |
| `?` | Toggle keybindings cheat sheet overlay |
| `q` / `Ctrl+C` | Quit `ghostdu` |

---

## 🚀 Installation & Usage

### 1. Run Directly
```bash
# Scan current directory (defaults to . with zero flags)
ghostdu

# Scan a specific directory
ghostdu /var/log

# Non-interactive summary report (or when piped to cat / grep)
ghostdu --summary /home/ron
```

### 2. Build from Source
```bash
git clone https://github.com/ron/ghostdu.git
cd ghostdu
cargo build --release

# Binary will be at target/release/ghostdu (1.3 MB)
# To install locally:
cp target/release/ghostdu ~/.local/bin/
```

---

## 🧪 Testing & Verification

Run automated test suite:
```bash
cargo test
```

Includes unit tests and integration tests verifying:
- Virtual filesystem automatic exclusion
- Ghost and cache path classification
- Inode deduplication on hard links
- Safe wastebin operations
- Permanent recursive deletions
- State machine navigation, selection, and filtering

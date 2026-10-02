# 👻 ghostdu

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-Linux-orange.svg)]()
[![Binary Size](https://img.shields.io/badge/binary-1.4_MB-purple.svg)]()

> **Modern, blazingly fast, native Linux disk usage & ghost file analyzer with wastebin support, smart deletion safety tiers, and zero-flag headaches.**

`ghostdu` is an ultra-fast, compact (1.4 MB native binary) terminal disk usage analyzer engineered in Rust. Inspired by `ncdu`, it is designed to eliminate common frustrations: no need to remember complex exclusion flags, automatic isolation of Docker and ghost artifacts, 4-tier deletion safety badges, native FreeDesktop wastebin/trash integration, and responsive rendering down to the smallest terminal window.

---

## ⚡ Key Highlights

### 🛡️ Smart Deletion Safety System & Guardrails
- **4 Safety Tiers**: Every file and folder is evaluated and labeled with an intuitive color-coded safety glyph:
  - 🟢 **`SAFE`**: Caches (`~/.cache`, build artifacts `target/`, `.pytest_cache`), FreeDesktop trash, rotated logs (`*.log`), crash dumps, browser caches, and editor temp files (`.tmp`, `.bak`, `.swp`).
  - 🟡 **`RECHECK`**: Dependency trees (`node_modules`, `.venv`), local AI model weights (`*.safetensors`, `*.gguf`), ISO images, VM disks, system snapshots, and Wine prefixes. Reproducible, but redownloading or rebuilding takes time and bandwidth.
  - ⚪ **`USER`**: Personal documents, source code, repositories, and private user files.
  - 🔴 **`SYSTEM`**: Protected Linux operating system roots (`/`, `/bin`, `/boot`, `/etc`, `/lib`, `/usr`, `/tmp` mount). **Deletion is actively blocked**.
- **Safe-to-Clean Quick Filter (`c`)**: Press `c` anytime to instantly filter the view to show only 🟢 **`SAFE`** cleanable items.
- **Active Deletion Guardrails**: Any attempt to delete critical system directories triggers an active safeguard lock—the confirmation key `y` is disarmed to prevent catastrophic system damage.

### 🏷️ 16 Linux Disk Category Badges
High-impact disk consumers across modern Linux desktop and developer environments are automatically recognized and badged:
- `🗑️ TRASH` — Wastebin & FreeDesktop trash
- `📜 LOGS` — System & application logs (`/var/log`, journal)
- `📦 FLATPAK` — Flatpak apps & runtime runtimes
- `📦 SNAP` — Snap packages & version revisions
- `📦 DEPS` — Project dependency trees (`node_modules`, `.venv`, `vendor/`)
- `🎮 GAME` — Steam shader caches, Proton compatdata, Wine prefixes
- `🤖 AI` — Local AI/LLM weights (Ollama, Hugging Face, GGUF, Safetensors)
- `💿 VM/ISO` — Virtual disks (`.qcow2`, `.vdi`) and installer ISOs
- `🌐 BROWSER` — Web browser caches (Chrome, Firefox, Brave, Edge, Discord, Slack)
- `💥 CRASH` — Systemd core dumps and crash reports
- `🔒 SNAP` — Timeshift and Snapper snapshots
- `🐳 DOCKER` — Docker images, containers, volumes, and BuildKit caches
- `🦭 PODMAN` — Rootless Podman container storage
- `👻 GHOST` — Unlinked open files held open by active processes
- `📦 PKG` — Package manager archives (`pacman`, `apt`, `dnf`, `yay`, `paru`)
- `👻 CACHE` — Build, compiler, and thumbnail caches (`target/`, `.cache`)

### 🐳 Docker & Unlinked Open Ghost Files
- **Docker Engine Direct Inspection**: Communicates directly with `/var/run/docker.sock` to report active vs reclaimable images, stopped containers, dangling volumes, and BuildKit caches.
- **Open Unlinked Ghost File Discovery**: Scans `/proc/*/fd` to expose deleted files that are still held open by active processes and silently consuming disk space.
- **Dedicated Ghost Inspector (`Tab` / `g`)**: Dedicated panel showing Docker storage breakdown and open deleted files with one-touch pruning (`p`).
- **Live Filter Toggles (`G`)**: In the explorer tree, press `G` to cycle between *Show All*, *Hide Ghost Files*, or *Ghost Files ONLY*.

### 🗑️ Wastebin & Permanent Deletion
- **Move to Wastebin (`w` / `t`)**: FreeDesktop.org Trash specification compliant (`~/.local/share/Trash`). Safe, recoverable, and visible in Dolphin, Nautilus, Thunar, or `trash-cli`.
- **Completely Remove (`d` / `D`)**: Permanent deletion with safety confirmation modals, size preview, and batch processing.
- **Multi-Selection (`Space` / `a`)**: Mark multiple files and directories across folders, with running counts and sizes.

### ℹ️ Global Overview & Detailed Metadata
- **Global Disk Overview Bar**: Shows root mount, filesystem type, total disk space, used space, free space, and safe reclaimable space.
- **Detailed Item Modal (`i`)**: Shows full inode metadata, allocated block count (`st_blocks * 512`), apparent size, device ID, exact permissions, and safety classification guide.
- **In-Place Refresh (`r` / `R`)**: Refresh directory contents or the entire tree without restarting.

### 📱 Responsive & Ultra-Lightweight
- **1.4 MB standalone native binary** (stripped, LTO enabled, zero runtime overhead).
- Parallel background scanning via Rayon with real-time animated progress.
- Dynamically adapts from widescreen displays down to tiny 20x3 terminal windows.
- Non-interactive summary mode for scripts (`ghostdu --summary` or piped output).

---

## ⌨️ Keyboard Shortcuts

| Key | Action |
| :--- | :--- |
| **Navigation** | |
| `j` / `Down` | Move cursor down |
| `k` / `Up` | Move cursor up |
| `Enter` / `l` / `Right` | Enter directory / drill down |
| `Backspace` / `h` / `Left` | Go up to parent directory |
| `Home` / `g` | Jump to top |
| `End` / `G` (with shift) | Jump to bottom |
| **Selection & Deletion** | |
| `Space` | Toggle selection on current item |
| `a` | Select all / unselect all visible items |
| `w` / `t` | **Move selected (or current) to Wastebin / Trash** |
| `d` / `D` | **Completely Remove (Permanent Delete)** |
| **Safety & Category Filters** | |
| `c` | **Toggle Safe-to-Clean ONLY filter** (`[🟢 SAFE ONLY]`) |
| `G` | **Cycle Ghost filter** (`All` → `Hide Ghost` → `Ghost ONLY`) |
| **Inspector & Metadata** | |
| `Tab` | Switch between Explorer & Ghost/Docker Inspector |
| `i` | Open detailed item info modal (ncdu style) |
| `1` / `2` | Switch tabs in Ghost Inspector (Docker vs Deleted Files) |
| `p` | Prune Docker dangling resources (in Ghost Inspector) |
| **Search, Sort & Refresh** | |
| `/` | Interactive live search / filter (type to match, `Esc` to clear) |
| `s` | Cycle sort order (*Size desc*, *Size asc*, *Name*, *Item count*) |
| `A` | Toggle Apparent size vs Actual block disk usage |
| `r` | Refresh current directory |
| `R` | Rescan entire tree from root |
| `?` | Toggle keybindings cheat sheet overlay |
| `q` / `Ctrl+C` | Quit `ghostdu` |

---

## 🚀 Installation & Usage

### 1. Build and Install from Source

Ensure you have Rust and Cargo installed:

```bash
# Clone the repository
git clone https://github.com/ronimuliawan-coder/ghostdu.git
cd ghostdu

# Install directly to ~/.cargo/bin (or ~/.local/bin)
cargo install --path .

# Or build an optimized release binary manually
cargo build --release
install -m 755 target/release/ghostdu ~/.local/bin/ghostdu
```

### 2. Command Line Usage

```bash
# Scan current directory (defaults to . with zero flags)
ghostdu

# Scan a specific directory
ghostdu /var/log

# Scan root filesystem
ghostdu /

# Non-interactive summary report (or when piped to cat / grep)
ghostdu --summary ~
```

---

## 🧪 Testing & Verification

Run the comprehensive unit and integration test suite:

```bash
cargo test
```

All 14 unit and integration tests verify:
- Automatic virtual filesystem exclusion (`/proc`, `/sys`, `/dev`, `/run`)
- Inode deduplication on hard links
- Safety tier classification and system deletion guardrails
- FreeDesktop wastebin operations
- Permanent recursive deletions
- State machine navigation, selection, and filtering
- Responsive TUI rendering across compact and wide dimensions

---

## 📄 License

This project is licensed under the [MIT License](LICENSE) - see the LICENSE file for details.

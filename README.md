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

Permanent deletion and trash moves use Linux directory handles and refuse to cross mount points, including bind mounts. They require Linux 5.6+ (`openat2`) and fail closed if the kernel or sandbox cannot provide these safeguards. Existing mounted descendants are checked before removal. Filesystem changes or I/O errors can still cause partial deletion; failed permanent deletions rescan only the affected targets, update ancestor totals, and show an error. Unrelated subtrees remain cached until explicitly refreshed. Reconciliation still runs synchronously within each affected subtree, so a very large failed target can take time. This is not a rollback mechanism.

Trash preflight checks mount topology for nested mounts without opening ordinary descendants; like the identity checks, this is a point-in-time check and does not serialize concurrent mount changes. Trash moves write FreeDesktop `.trashinfo` metadata and use an atomic rename between pinned directories. Home and per-volume trash locations must be private and owned by the current user. Unsupported cross-filesystem moves fail without copying or removing the source.

Selections and confirmation dialogs hold open handles to the selected objects. Selections are capped at 256 and further limited by the live file-descriptor budget, reserving headroom for operations. Select-all reports how many items were selected when it reaches that limit. Before either destructive operation, the backend compares the prepared target with the confirmed object. A replacement observed by these checks requires a new selection and confirmation; refreshing discards changed selections. Failure to capture or verify an identity blocks that target. This binds the selected object, not a snapshot of its contents: files can still be edited and directory children can change while the dialog is open.

The identity guarantee applies at verification time. Linux name-based `unlinkat` and `renameat` are separate from the final identity check: a concurrent writer can replace the final entry in that interval, and the replacement may be removed or trashed. Pinned parents prevent ancestor redirection and final symlinks are not followed, but these controls do not serialize other writers. Deterministic tests replace the final entry after verification to record this boundary. Stronger guarantees require control of the directory namespace (exclusive write authority or coordination honored by every writer); another metadata check alone cannot provide them. Deployments needing that stronger guarantee must limit destructive actions to namespaces without untrusted writers.

Batch failure reconciliation uses indexed root lookups and a tree-update walk pruned to ancestors of changed paths. Batch trash moves retain validated destination handles per source mount; private ownership and permissions are rechecked for every target. Kernels without mount IDs use uncached destination resolution so bind mounts cannot share a destination by device ID alone.

Shell (`!`), desktop-open (`o`), and clipboard (`y`) handoffs run with the invoking user's own authority and environment — nothing is interpolated into a shell string, and nothing is sandboxed or dropped. They are conveniences of an interactive local tool, not privilege boundaries: do not run ghostdu with authority you would not grant to your own shell.

#### Trash interruption and recovery

The restore metadata is written and synced before the source is renamed into `Trash/files`. For process termination (with the filesystem still running), the recoverable states are:

| Interruption point | Source | Trash state / recovery |
| --- | --- | --- |
| Before metadata is complete | Still at its original path | An empty or partial orphan `.trashinfo` may remain. |
| After metadata sync, before rename | Still at its original path | Complete orphan metadata may remain, with no matching payload. |
| After successful rename | In `Trash/files` | Complete `.trashinfo` and payload share the same stored name; restore with a FreeDesktop consumer such as `trash-restore`. |

On a reported failure before rename, ghostdu attempts to remove the reserved metadata. If cleanup also fails, the error includes the original failure and the orphan metadata location. Inspect an orphan and confirm there is no matching `Trash/files` payload before removing it manually. Ghostdu does not automatically sweep orphans or overwrite an existing restore destination. A successful rename retains its metadata, even if the process exits before the UI reports success.

This contract covers process interruption, not power loss or storage failure: the operation does not sync directory entries or the source contents and cannot promise crash-durable transactions across a reboot. The regression tests exit subprocesses at each boundary and check recovery states. External restoration of files, directories, symlinks, and escaped names is verified with `trash-cli` 0.24.5.26. To run that optional interoperability test with an installed consumer:

```sh
GHOSTDU_TRASH_RESTORE=/path/to/trash-restore cargo test --lib external_consumer_restores -- --ignored
```

Reclaimable-space estimates conservatively exclude files with multiple hard links, even if all links appear in the scan. Disk-usage totals still count their blocks once.

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
- **Docker authority**: Reporting and pruning use only `/var/run/docker.sock` under the invoking user's existing socket permissions. Pruning affects eligible resources across that daemon. Ghostdu does not change socket permissions, elevate privileges, or switch to the Docker CLI's configured context.
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

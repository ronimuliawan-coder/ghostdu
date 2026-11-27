# Changelog

All notable changes to `ghostdu` are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries describe the effect on the user, not the shape of the diff. Each links the pull request
that carried it. Verification steps for anything in this file live in [`docs/QA.md`](docs/QA.md).

**Adding an entry:** put it under `## [Unreleased]` in the matching category, in the same pull
request as the change. See `docs/QA.md` §6.

---

## [Unreleased]

Merged to `main` after the `v0.1.1` release and not yet tagged. The next tag will be the first to
exercise the release workflow's `verify` job and `download-artifact@v8` through the publish path.

### Added

- A manual verification guide at `docs/QA.md`, and this changelog. (`docs/` is excluded from git in
  this repository, so the guide is a local document; this changelog is tracked.)

### Fixed

- Deleting a deeply nested directory no longer aborts the process. The destructive walk was
  recursive, holding a stack frame per level, and a deep enough tree overflowed the stack — with
  `panic = "abort"` in release that is a fatal signal mid-deletion, so no result was returned and
  the view went on describing a partly deleted tree. Both walks are now iterative with one
  directory handle per level, and a target nested deeper than 512 levels is refused **before**
  anything is unlinked, leaving the tree intact. (#27)
- Trash moves are durable in the right order. The `.trashinfo` directory is flushed after the
  metadata is written and before the rename, then the source parent and `files/` after it. A
  durability shortfall after a successful rename is now reported as a warning rather than a
  failure, so an entry that genuinely moved is no longer shown as failed and left in the tree. (#27)
- An unusable shared `.Trash` directory no longer falls back silently. The cause (for example a
  full or read-only shared trash) is carried to the status line, while the per-user `.Trash-<uid>`
  fallback the freedesktop spec allows is still used. (#27)
- A status line for a mixed batch now reports the destination warning alongside the failure
  reason, instead of hiding it whenever any item failed. (#27)

### Changed

- Commit-message law allows exactly one attribution trailer: Dependabot's machine sign-off
  `Signed-off-by: dependabot[bot] <support@github.com>`. GitHub credits only the commit author and
  `Co-authored-by:` trailers, so that line cannot alter the contributor list. Every other trailer
  remains rejected, and `Co-authored-by:` remains forbidden for all identities. (#26)

---

## [0.1.1] — 2026-11-25

Deletion-guardrail integrity, a safe Docker prune, and a non-blocking refresh.

### Added

- **Itemized Docker prune.** `p` now presents each reclaimable category with its own size and
  toggle instead of one confirmation that pruned everything. Dangling images default on; stopped
  containers, anonymous volumes and the build cache default off; an empty selection refuses rather
  than doing nothing silently. Named-volume totals are labelled as an upper bound, because a
  default prune keeps volumes newer than the daemon's filter. (#12)
- `SECURITY.md` with a private reporting channel, and a declared MSRV (`rust-version = "1.99.1"`)
  that matches the toolchain CI actually tests. (#16)
- Dependabot configuration: weekly Cargo and GitHub Actions updates, minor and patch grouped so a
  routine wave is one pull request, with the toolchain deliberately left out because the Cargo
  ecosystem cannot see `rust-toolchain.toml`. (#16)
- Manual dispatch for the release workflow, with `publish` gated to tag pushes only, plus a
  `verify` job that checksums, lists and extracts both archives and runs `--version` before
  anything is published. (#24)

### Changed

- **Refresh no longer freezes the UI.** `r` moved off the UI thread onto the existing
  background-scan path with progress and cancellation. Measured: a release-mode root rescan froze
  the interface for 3.5 s; it now paints progress while it works and keeps the previous tree if the
  scan fails. (#13)
- Startup no longer blocks on Docker or a `/proc` walk; that data loads on first inspector open.
  First frame measured at 142 ms. (#16)
- Test coverage is enforced at 100% by CI (`cargo tarpaulin --fail-under 100`). (#16)

### Fixed

- **Deletion guardrails against bind-mount aliases.** A bind mount of a protected directory could
  bypass the path-based guardrail and let `/etc` be deleted through its alias. Deletion now
  compares `(device, inode)` identity against protected directories, walks ancestors with a
  per-batch mount table, and fails closed on stacked mounts. Also closed: `/lib32` missing from the
  protected set, substring classification such as `/usr` matching `cache`, `/var/log` containment,
  and the `.cache` root losing its badge. (#11)
- Docker client no longer hangs on a stalled daemon: per-read deadlines are clamped to the time
  remaining before a wall-clock deadline, and responses are capped at 64 MiB. (#12)
- Terminal escape sequences in file names, Docker names and error strings can no longer reach the
  terminal; one sanitizer now runs at every render boundary, including the headless report and
  `--diff` output. (#12)
- Item-info disk usage was unhelpful for two entry kinds: directories showed their own inode blocks
  rather than the scanned subtree total, and zero-block files showed `0 B` with no explanation. It
  now reports the subtree total for directories, and the inode figure plus `(sparse or inline)` for
  zero-block files. (#16)
- Directory totals no longer lose the directory's own allocation when children are re-summed. (#16)
- Empty files are no longer labelled as sparse or inline. (#16)

### Packaging

- `v0.1.1` published with all four assets; both `PKGBUILD`s carry the checksums of the **published
  artifacts**, which differ per package (`ghostdu` builds from GitHub's tag archive, `ghostdu-bin`
  from the release asset). A version bump and its sums now move in the same commit. (#15)

---

## [0.1.0] — 2026-10-17

First release: Btrfs CoW sharing and AUR packaging on top of the feature roadmap.

### Added

- **Snapshot export, import and diff.** `--export` writes the scan tree as versioned JSON;
  `--import` browses a snapshot read-only with destructive actions disabled; `diff` reports growth
  deltas between two snapshots. (#5)
- **Btrfs CoW sharing** behind `--cow`, via FIEMAP, showing per-file shared extents in the item
  panel. (#6)
- **File-age heatmap**, age-based sorting, and a stale-only filter for items untouched for 180
  days. (#5)
- **Btrfs subvolume labels** and a snapshot warning for timeshift/snapper layouts. (#5)
- **AUR packaging** (`ghostdu` source package and `ghostdu-bin`) and a release pipeline producing
  x86_64 and aarch64 tarballs with SHA-256 checksums. (#6, #7)
- Shell completions (bash, elvish, fish, powershell, zsh) and a generated man page. (#3)
- Janitor view, top-50 largest files, subshell/open/copy helpers, and process termination for
  deleted-open files. (#2, #3)

### Changed

- 100% line coverage became a hard CI gate, with the live pty test exempted because it hangs under
  instrumentation. (#4)

### Fixed

- FIEMAP `ioctl` retries on `EINTR`; CoW values are labelled as logical sizes. (#6)
- Snapshot import gates live-data refresh and CoW queries, which are meaningless for a recorded
  tree. (#6)
- Release workflow publishes reliably: the tag is materialised for `--notes-from-tag`, ref
  auto-follow is avoided, and the publish job holds least privilege. (#6, #7)

---

## Not yet released, on `main`

Commits merged after `v0.1.1` that are not part of a tag:

| Change | PR |
| --- | --- |
| Dependabot maintenance wave (Cargo and GitHub Actions) | #17–#21, #25 |
| ratatui 0.30 and crossterm 0.29 upgraded together, since the two cannot land separately | #24 |
| Release workflow: manual dry run, `--repo` fix, and two review-found gaps closed | #24 |
| History rewrite removing attribution trailers, with the source-tarball checksum rebound | — |
| Deletion-path safety: bounded traversal, durable trash, visible fallback cause | #27 |
| Commit-message law: Dependabot machine sign-off allowed | #26 |

The next tag will be the first to exercise the release workflow's `verify` job and
`download-artifact@v8` through the publish path.

---

## Recorded limitations

Known and tracked, not bugs to re-report. Detail in `docs/QA.md` §7.

- Directory totals can be incomplete at a `--depth` cap, a skipped mount, or a cycle, and are not
  yet qualified in the UI.
- Snapshot import has no size cap, and `diff` holds two trees in memory.
- The scanner is single-threaded; parallelization is benchmark-gated.
- `--exclude` matches substrings only, with no glob or negation support.
- `y` waits for the clipboard tool to exit, briefly occupying the UI thread.

//! Snapshot diffing: compare two exported trees path by path.
//!
//! Sizes are on-disk bytes (`disk_usage`), so directory rollups reflect real
//! block consumption. All lists sort by delta magnitude, then path, so output
//! is deterministic across runs.

use crate::fs::entry::FileEntry;
use std::collections::HashMap;
use std::path::PathBuf;

/// One path whose on-disk size changed between snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub path: PathBuf,
    pub before_bytes: u64,
    pub after_bytes: u64,
}

impl Delta {
    fn change(&self) -> i64 {
        self.after_bytes as i64 - self.before_bytes as i64
    }
}

/// Categorized path-by-path comparison of two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffReport {
    /// Largest added paths (up to 10).
    pub added: Vec<Delta>,
    /// Largest removed paths (up to 10).
    pub removed: Vec<Delta>,
    /// Paths in both that grew, largest growth first (up to 10).
    pub grown: Vec<Delta>,
    /// Paths in both that shrank, largest shrinkage first (up to 10).
    pub shrunk: Vec<Delta>,
    /// Every added path and their bytes: the lists above keep only the top
    /// 10, but the header reports the full totals.
    pub added_count: usize,
    pub added_bytes: u64,
    pub removed_count: usize,
    pub removed_bytes: u64,
}

fn flatten(root: &FileEntry, map: &mut HashMap<PathBuf, u64>) {
    map.insert(root.path.clone(), root.disk_usage);
    for child in &root.children {
        flatten(child, map);
    }
}

/// Compare two scanned trees. Unchanged paths are dropped; every list keeps
/// the 10 largest by magnitude (then path) via linear-time selection, and
/// the header counts every changed path, not just the listed ones.
pub fn diff_trees(before: &FileEntry, after: &FileEntry) -> DiffReport {
    let mut before_map = HashMap::new();
    let mut after_map = HashMap::new();
    flatten(before, &mut before_map);
    flatten(after, &mut after_map);

    let mut report = DiffReport::default();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut grown = Vec::new();
    let mut shrunk = Vec::new();
    for (path, &after_bytes) in &after_map {
        match before_map.get(path) {
            None => added.push(Delta {
                path: path.clone(),
                before_bytes: 0,
                after_bytes,
            }),
            Some(&before_bytes) if after_bytes > before_bytes => grown.push(Delta {
                path: path.clone(),
                before_bytes,
                after_bytes,
            }),
            Some(&before_bytes) if after_bytes < before_bytes => shrunk.push(Delta {
                path: path.clone(),
                before_bytes,
                after_bytes,
            }),
            Some(_) => {}
        }
    }
    for (path, &before_bytes) in &before_map {
        if !after_map.contains_key(path) {
            removed.push(Delta {
                path: path.clone(),
                before_bytes,
                after_bytes: 0,
            });
        }
    }
    report.added_count = added.len();
    report.added_bytes = added.iter().map(|d| d.after_bytes).sum();
    report.removed_count = removed.len();
    report.removed_bytes = removed.iter().map(|d| d.before_bytes).sum();
    top10(&mut added, |a, b| {
        b.after_bytes
            .cmp(&a.after_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    top10(&mut removed, |a, b| {
        b.before_bytes
            .cmp(&a.before_bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    top10(&mut grown, |a, b| {
        b.change()
            .cmp(&a.change())
            .then_with(|| a.path.cmp(&b.path))
    });
    top10(&mut shrunk, |a, b| {
        a.change()
            .cmp(&b.change())
            .then_with(|| a.path.cmp(&b.path))
    });
    report.added = added;
    report.removed = removed;
    report.grown = grown;
    report.shrunk = shrunk;
    report
}

/// Keep the 10 largest by `cmp`, ordered. Linear-time selection beats a full
/// sort for snapshot-scale lists; the sort afterwards only orders the 10.
fn top10(v: &mut Vec<Delta>, cmp: impl Fn(&Delta, &Delta) -> std::cmp::Ordering) {
    if v.len() > 10 {
        v.select_nth_unstable_by(10, &cmp);
        v.truncate(10);
    }
    v.sort_by(cmp);
}

fn format_delta_list(title: &str, deltas: &[Delta], out: &mut String) {
    use std::fmt::Write;
    out.push_str(title);
    for delta in deltas {
        let sign = if delta.change() >= 0 { "+" } else { "-" };
        let magnitude = delta.change().unsigned_abs();
        let _ = writeln!(
            out,
            "\n  {}{} in {}",
            sign,
            crate::fs::entry::format_size(magnitude),
            delta.path.display()
        );
    }
    out.push('\n');
}

impl std::fmt::Display for DiffReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        out.push_str(&format!(
            "Added {} paths (+{}), removed {} paths (-{})\n",
            self.added_count,
            crate::fs::entry::format_size(self.added_bytes),
            self.removed_count,
            crate::fs::entry::format_size(self.removed_bytes)
        ));
        format_delta_list("Grew:", &self.grown, &mut out);
        format_delta_list("Shrank:", &self.shrunk, &mut out);
        write!(f, "{out}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::entry::{DeleteSafety, GhostKind};

    fn file(path: &str, disk_usage: u64) -> FileEntry {
        FileEntry::new_file(
            path.to_string(),
            PathBuf::from(path),
            disk_usage,
            disk_usage,
            false,
            1,
            100,
            GhostKind::None,
            DeleteSafety::UserData,
        )
    }

    fn tree(children: Vec<FileEntry>) -> FileEntry {
        let mut root = FileEntry::new_dir(
            "root".to_string(),
            PathBuf::from("/root"),
            1,
            1,
            GhostKind::None,
            DeleteSafety::UserData,
        );
        root.children = children;
        root.disk_usage = root.children.iter().map(|c| c.disk_usage).sum();
        root
    }

    #[test]
    fn diff_categorizes_every_change_kind() {
        let before_children = vec![
            file("/root/gone.txt", 100),
            file("/root/gone2.txt", 200),
            file("/root/grow.txt", 100),
            file("/root/same.txt", 100),
            file("/root/shrink2.txt", 200),
            file("/root/steady.txt", 100),
        ];
        let before = tree(before_children);
        let after = tree(vec![
            file("/root/grow.txt", 300),
            file("/root/new.txt", 50),
            file("/root/new2.txt", 60),
            file("/root/same.txt", 50),
            file("/root/shrink2.txt", 80),
            file("/root/steady.txt", 100),
        ]);
        let report = diff_trees(&before, &after);
        assert_eq!(report.added.len(), 2);
        assert_eq!(report.added[0].path, PathBuf::from("/root/new2.txt"));
        assert_eq!(report.removed.len(), 2);
        assert_eq!(report.removed[0].path, PathBuf::from("/root/gone2.txt"));
        assert_eq!(report.grown.len(), 1);
        assert_eq!(report.grown[0].path, PathBuf::from("/root/grow.txt"));
        assert_eq!(report.grown[0].change(), 200);
        assert_eq!(report.shrunk.len(), 3);
        assert_eq!(report.shrunk[0].path, PathBuf::from("/root"));
        assert_eq!(report.shrunk[1].path, PathBuf::from("/root/shrink2.txt"));
        assert_eq!(report.shrunk[2].path, PathBuf::from("/root/same.txt"));
        let text = report.to_string();
        assert!(text.contains("Added 2 paths"));
        assert!(text.contains("removed 2 paths"));
        assert!(text.contains("/root/grow.txt"));
    }

    #[test]
    fn header_totals_cover_unlisted_paths() {
        let before = tree(vec![]);
        let after = tree(
            (0..12u64)
                .map(|i| file(&format!("/root/f{i:02}.txt"), 100 + i))
                .collect(),
        );
        let report = diff_trees(&before, &after);
        assert_eq!(report.added.len(), 10);
        assert_eq!(report.added_count, 12);
        assert_eq!(report.added_bytes, (0..12u64).map(|i| 100 + i).sum::<u64>());
        assert_eq!(report.added[0].after_bytes, 111);
        let text = report.to_string();
        assert!(text.contains("Added 12 paths"));
    }
}

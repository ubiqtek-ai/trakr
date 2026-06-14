//! Archive sweep — copies Claude transcript files to `~/.trakr/archive/`.
//!
//! The archive is a mirror of `~/.claude/projects/` and grows monotonically:
//! files are only ever added or updated, never deleted.  A copy is taken when
//! the destination is missing or when its (size, mtime) differ from the source.
//!
//! Copying is done via a temp file + atomic rename so a partial write never
//! leaves a corrupt destination file.  The source mtime is preserved on the
//! copy so that subsequent sweeps see a stable baseline and skip unchanged files.

use anyhow::{Context, Result};
use filetime::FileTime;
use std::fs;
use std::path::{Path, PathBuf};

/// Statistics returned by a single [`run_archive_sweep`] call.
#[derive(Debug, Default)]
pub struct ArchiveStats {
    /// Number of files copied (new or changed).
    pub copied: usize,
    /// Number of files skipped because dest was already up to date.
    pub unchanged: usize,
    /// Total bytes written during this sweep.
    pub bytes_copied: u64,
}

/// Run one archive sweep: walk `src` and mirror matching files under `dest`.
///
/// Files included:
/// - `<src>/<slug>/<uuid>.jsonl` — main session files (depth 1 under each project slug).
/// - `<src>/<slug>/<uuid>/subagents/<file>.jsonl` — subagent transcripts.
///
/// Files are copied when the destination is absent or when (size, mtime) differ
/// from the source.  The source mtime is stamped onto the copy after writing.
///
/// `dest` must already exist (call `fs::create_dir_all` before calling this).
pub fn run_archive_sweep(src: &Path, dest: &Path) -> Result<ArchiveStats> {
    let mut stats = ArchiveStats::default();

    // Walk: src/<slug>/
    let slug_entries = match fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) => {
            return Err(e).with_context(|| format!("reading source dir {}", src.display()));
        }
    };

    for slug_entry in slug_entries.filter_map(|e| e.ok()) {
        let slug_path = slug_entry.path();
        if !slug_path.is_dir() {
            continue;
        }

        // Walk: src/<slug>/<uuid>.jsonl  (main session files)
        // and:  src/<slug>/<uuid>/subagents/*.jsonl
        let session_entries = match fs::read_dir(&slug_path) {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        for session_entry in session_entries.filter_map(|e| e.ok()) {
            let session_path = session_entry.path();

            if session_path.is_file()
                && session_path.extension().map_or(false, |e| e == "jsonl")
            {
                // Main session file: <slug>/<uuid>.jsonl
                let rel = relative_path(src, &session_path);
                maybe_copy(&session_path, &dest.join(&rel), &mut stats)
                    .with_context(|| format!("copying {}", session_path.display()))?;
            } else if session_path.is_dir() {
                // Possible <slug>/<uuid>/ directory — look for subagents/.
                let subagents_dir = session_path.join("subagents");
                if subagents_dir.is_dir() {
                    let sub_entries = match fs::read_dir(&subagents_dir) {
                        Ok(rd) => rd,
                        Err(_) => continue,
                    };
                    for sub_entry in sub_entries.filter_map(|e| e.ok()) {
                        let sub_path = sub_entry.path();
                        if sub_path.is_file()
                            && sub_path.extension().map_or(false, |e| e == "jsonl")
                        {
                            let rel = relative_path(src, &sub_path);
                            maybe_copy(&sub_path, &dest.join(&rel), &mut stats)
                                .with_context(|| format!("copying {}", sub_path.display()))?;
                        }
                    }
                }
            }
        }
    }

    Ok(stats)
}

/// Return the relative path of `target` with respect to `base`.
///
/// Panics if `target` does not start with `base` — callers guarantee this.
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    target.strip_prefix(base).expect("target must be under base").to_path_buf()
}

/// Copy `src_path` to `dest_path` if needed; update `stats`.
///
/// "Needed" means: dest is absent **or** (size, mtime) differ from src.
/// Uses a temp file + atomic rename; preserves source mtime on the copy.
fn maybe_copy(src_path: &Path, dest_path: &Path, stats: &mut ArchiveStats) -> Result<()> {
    let src_meta = fs::metadata(src_path)
        .with_context(|| format!("stat {}", src_path.display()))?;
    let src_size = src_meta.len();
    let src_mtime = FileTime::from_last_modification_time(&src_meta);

    // Check whether dest is already up to date.
    if let Ok(dest_meta) = fs::metadata(dest_path) {
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        if dest_meta.len() == src_size && dest_mtime == src_mtime {
            stats.unchanged += 1;
            return Ok(());
        }
    }

    // Ensure the destination parent directory exists.
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating archive dir {}", parent.display()))?;
    }

    // Write to a temp file in the same directory, then atomically rename.
    let tmp_path = dest_path.with_extension("tmp");
    fs::copy(src_path, &tmp_path)
        .with_context(|| format!("copying to tmp {}", tmp_path.display()))?;

    // Stamp the source mtime onto the copy so future sweeps see a stable baseline.
    filetime::set_file_times(&tmp_path, src_mtime, src_mtime)
        .with_context(|| format!("setting mtime on {}", tmp_path.display()))?;

    fs::rename(&tmp_path, dest_path)
        .with_context(|| format!("renaming {} → {}", tmp_path.display(), dest_path.display()))?;

    stats.copied += 1;
    stats.bytes_copied += src_size;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::tempdir;

    /// Build a source tree with two main session files and one subagent file:
    ///
    /// ```text
    /// src/
    ///   slug-a/
    ///     session-1.jsonl
    ///     session-2.jsonl
    ///     session-1/
    ///       subagents/
    ///         agent-x.jsonl
    /// ```
    fn build_source_tree(src: &Path) {
        let slug = src.join("slug-a");
        fs::create_dir_all(&slug).unwrap();

        // Main session files.
        fs::write(slug.join("session-1.jsonl"), b"line1\n").unwrap();
        fs::write(slug.join("session-2.jsonl"), b"line2\n").unwrap();

        // Subagent file.
        let subagents = slug.join("session-1").join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::write(subagents.join("agent-x.jsonl"), b"agent_line\n").unwrap();
    }

    #[test]
    fn test_basic_sweep_copies_all_files() {
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();

        build_source_tree(src_dir.path());

        let stats = run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();

        assert_eq!(stats.copied, 3, "all 3 files should be copied");
        assert_eq!(stats.unchanged, 0);

        // Verify each file exists in dest.
        assert!(dest_dir.path().join("slug-a/session-1.jsonl").exists());
        assert!(dest_dir.path().join("slug-a/session-2.jsonl").exists());
        assert!(dest_dir.path().join("slug-a/session-1/subagents/agent-x.jsonl").exists());
    }

    #[test]
    fn test_idempotent_second_sweep_copies_nothing() {
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();

        build_source_tree(src_dir.path());

        // First sweep.
        let stats1 = run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();
        assert_eq!(stats1.copied, 3);

        // Second sweep on unchanged tree.
        let stats2 = run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();
        assert_eq!(stats2.copied, 0, "second sweep should copy nothing");
        assert_eq!(stats2.unchanged, 3, "all 3 files should be unchanged");
    }

    #[test]
    fn test_incremental_only_changed_file_is_recopied() {
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();

        build_source_tree(src_dir.path());

        // First sweep.
        let stats1 = run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();
        assert_eq!(stats1.copied, 3);

        // Append a byte to session-1.jsonl, bump its mtime by 2 s so the
        // comparison is stable regardless of filesystem mtime resolution.
        let modified_path = src_dir.path().join("slug-a/session-1.jsonl");
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&modified_path)
                .unwrap();
            f.write_all(b"x").unwrap();
        }
        // Advance mtime by 2 seconds to ensure the filesystem records a change.
        let new_mtime = FileTime::from_unix_time(
            FileTime::from_last_modification_time(&fs::metadata(&modified_path).unwrap())
                .unix_seconds()
                + 2,
            0,
        );
        filetime::set_file_times(&modified_path, new_mtime, new_mtime).unwrap();

        // Second sweep — only the modified file should be recopied.
        let stats2 = run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();
        assert_eq!(stats2.copied, 1, "only the changed file should be recopied");
        assert_eq!(stats2.unchanged, 2);
    }

    #[test]
    fn test_mtime_preserved_on_copy() {
        let src_dir = tempdir().unwrap();
        let dest_dir = tempdir().unwrap();

        build_source_tree(src_dir.path());

        run_archive_sweep(src_dir.path(), dest_dir.path()).unwrap();

        let src_path = src_dir.path().join("slug-a/session-1.jsonl");
        let dest_path = dest_dir.path().join("slug-a/session-1.jsonl");

        let src_mtime =
            FileTime::from_last_modification_time(&fs::metadata(&src_path).unwrap())
                .unix_seconds();
        let dest_mtime =
            FileTime::from_last_modification_time(&fs::metadata(&dest_path).unwrap())
                .unix_seconds();

        // Mtime must match within 1 second tolerance.
        assert!(
            (src_mtime - dest_mtime).abs() <= 1,
            "dest mtime ({dest_mtime}) should match src mtime ({src_mtime}) within 1 s"
        );
    }
}

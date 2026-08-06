//! Workspace-path completion for `@` references in the composer.

use std::fs;
use std::path::{Path, PathBuf};

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

/// A workspace-relative path that can be inserted after `@`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathCandidate {
    pub path: String,
    pub is_dir: bool,
}

/// Collect workspace paths without walking generated or VCS metadata. Keeping
/// this bounded makes opening the completion menu predictable in large repos.
pub fn collect_workspace_paths(root: &Path) -> Vec<PathCandidate> {
    const MAX_CANDIDATES: usize = 4_000;
    let mut paths = Vec::new();
    collect(root, root, &mut paths, MAX_CANDIDATES);
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    paths
}

/// Collect paths starting from `root`, skipping generated/VCS metadata.
/// `max_depth` caps how deep we descend (0 = root only, 1 = root + one level, …);
/// `max_candidates` caps the total number returned.
pub fn collect_foreign_paths(root: &Path, max_candidates: usize, max_depth: u32) -> Vec<PathCandidate> {
    let mut paths = Vec::new();
    if !root.exists() || !root.is_dir() {
        return paths;
    }
    collect_from(root, root, &mut paths, max_candidates, max_depth);
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    paths
}

/// Resolve an `@`-typed foreign path and return completions as **absolute**
/// paths ready to insert.
///
/// Supports absolute paths (`@/usr/…`) and home-relative ones (`@~/…`, `@~`).
/// It expands `~`, walks down the deepest *existing* ancestor (so a partially
/// typed query still lists a real directory), fuzzy-matches only the residual
/// fragment beyond that ancestor against its contents, and prefixes each hit
/// back with the ancestor so insertion yields a fully expanded path.
pub fn foreign_match(query: &str, home: &Path) -> Vec<PathCandidate> {
    const MAX_CANDIDATES: usize = 500;
    const MAX_DEPTH: u32 = 3;

    // Build the absolute path the query points toward as a string so we avoid
    // `PathBuf::join`, which silently discards the parent path when given an
    // absolute (…"/…") component — the classic `~/` → `/` footgun.
    let home_str = home.to_string_lossy();
    let abs_str = if query == "~" || query.starts_with("~/") {
        if query == "~" {
            home_str.to_string()
        } else {
            format!("{}{}", home_str, &query[1..])
        }
    } else if query.starts_with('/') {
        query.to_string()
    } else {
        return Vec::new();
    };

    // Narrow to the deepest existing ancestor directory so a partial path such
    // as `/home/lumos1/wor` still scans `/home/lumos1` instead of failing.
    let mut so_far = PathBuf::from("/");
    for comp in PathBuf::from(&abs_str).components().skip(1) {
        let next = so_far.join(comp.as_os_str());
        if next.is_dir() {
            so_far = next;
        } else {
            break;
        }
    }
    let root_str = so_far.to_string_lossy().to_string();
    if !so_far.is_dir() {
        return Vec::new();
    }

    // The residual to fuzzy-match = whatever the user typed beyond `so_far`.
    let residual = abs_str
        .strip_prefix(&root_str)
        .map(|rest| rest.trim_start_matches('/'))
        .unwrap_or("");

    let candidates = collect_foreign_paths(&so_far, MAX_CANDIDATES, MAX_DEPTH);
    let filtered = fuzzy_paths(&candidates, residual);

    // Prefix hits back with the scanned root so the inserted text is a complete
    // absolute path.
    let separator = if root_str.ends_with('/') { "" } else { "/" };
    filtered
        .into_iter()
        .map(|mut candidate| {
            candidate.path = format!("{root_str}{separator}{}", candidate.path);
            candidate
        })
        .collect()
}

fn collect_from(root: &Path, dir: &Path, paths: &mut Vec<PathCandidate>, max: usize, depth: u32) {
    if paths.len() >= max || depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if paths.len() >= max {
            break;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if matches!(
            name.as_ref(),
            ".git" | "target" | "node_modules" | ".DS_Store"
        ) {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_dir = kind.is_dir();
        let mut display = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/").to_string());
        if is_dir {
            display.push('/');
        }
        paths.push(PathCandidate {
            path: display,
            is_dir,
        });
        if is_dir {
            collect_from(root, &path, paths, max, depth - 1);
        }
    }
}

fn collect(root: &Path, dir: &Path, paths: &mut Vec<PathCandidate>, max: usize) {
    if paths.len() >= max {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if paths.len() >= max {
            break;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        // These trees are either implementation metadata or commonly huge
        // generated dependency/build output, neither useful for an @ mention.
        if matches!(
            name.as_ref(),
            ".git" | "target" | "node_modules" | ".DS_Store"
        ) {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_dir = kind.is_dir();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let mut display = relative.to_string_lossy().replace('\\', "/");
        if is_dir {
            display.push('/');
        }
        paths.push(PathCandidate {
            path: display,
            is_dir,
        });
        if is_dir {
            collect(root, &path, paths, max);
        }
    }
}

/// Fuzzy-match and rank candidates. Empty queries retain a stable directory-
/// first alphabetical order; non-empty queries prioritize the fuzzy score.
pub fn fuzzy_paths(candidates: &[PathCandidate], query: &str) -> Vec<PathCandidate> {
    if query.is_empty() {
        let mut result = candidates.to_vec();
        result.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.path.cmp(&b.path)));
        return result;
    }

    let matcher = SkimMatcherV2::default();
    let query = query.to_lowercase();
    let mut ranked: Vec<(i64, &PathCandidate)> = candidates
        .iter()
        .filter_map(|candidate| {
            matcher
                .fuzzy_match(&candidate.path.to_lowercase(), &query)
                .map(|score| (score, candidate))
        })
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    ranked
        .into_iter()
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{foreign_match, fuzzy_paths, PathCandidate};
    use std::path::Path;

    fn candidate(path: &str, is_dir: bool) -> PathCandidate {
        PathCandidate {
            path: path.into(),
            is_dir,
        }
    }

    #[test]
    fn empty_query_lists_directories_before_files() {
        let results = fuzzy_paths(
            &[
                candidate("src/main.rs", false),
                candidate("src/", true),
                candidate("README.md", false),
            ],
            "",
        );
        assert_eq!(results[0].path, "src/");
    }

    #[test]
    fn fuzzy_query_prefers_a_tighter_match() {
        let results = fuzzy_paths(
            &[
                candidate("src/tui/app.rs", false),
                candidate("src/agent/mod.rs", false),
            ],
            "app",
        );
        assert_eq!(results[0].path, "src/tui/app.rs");
    }

    fn tmp_home(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn tmp_home3(name: &str) -> std::path::PathBuf {
        tmp_home(&std::env::temp_dir(), name)
    }

    #[test]
    fn foreign_tilde_lists_home_contents() {
        let home = tmp_home3("xa_foreign_home");
        std::fs::create_dir_all(home.join("work/scripts")).unwrap();
        std::fs::write(home.join("notes.md"), "x").unwrap();
        let results = foreign_match("~/", &home);
        let paths: Vec<_> = results.iter().map(|c| c.path.clone()).collect();
        assert!(paths.contains(&format!("{}/work/", home.display())));
        assert!(paths.contains(&format!("{}/notes.md", home.display())));
        assert!(paths.iter().all(|p| p.starts_with(home.to_str().unwrap())));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn foreign_tilde_fuzzy_matches_residual_only() {
        let home = tmp_home3("xa_foreign_home2");
        std::fs::create_dir_all(home.join("work/xa")).unwrap();
        let results = foreign_match("~/wor", &home);
        assert!(
            results
                .iter()
                .any(|c| c.path == format!("{}/work/", home.display())),
            "expected home/work/ in {:?}",
            results
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn foreign_absolute_walks_existing_ancestor() {
        let home = tmp_home3("xa_foreign_home3");
        let base = home.join("tree");
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/main.rs"), "fn main() {}").unwrap();
        let root = base.to_str().unwrap();
        let results = foreign_match(&format!("{root}/sr"), &home);
        assert!(
            results.iter().any(|c| c.path.ends_with("/src/")),
            "expected {}",
            results
                .iter()
                .map(|c| c.path.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

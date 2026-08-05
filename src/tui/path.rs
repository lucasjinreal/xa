//! Workspace-path completion for `@` references in the composer.

use std::fs;
use std::path::Path;

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
    use super::{fuzzy_paths, PathCandidate};

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
}

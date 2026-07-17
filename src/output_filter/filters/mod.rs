//! Command filter registry. Add new filters here without changing middleware.

pub mod common;
mod cargo;
mod default;
mod git;
mod python;
mod system;
mod tests;

type FilterFn = fn(&str) -> String;

struct Filter {
    name: &'static str,
    matches: fn(&str) -> bool,
    apply: FilterFn,
}

const FILTERS: &[Filter] = &[
    Filter { name: "git-diff", matches: git::is_diff, apply: git::filter_diff },
    Filter { name: "git-log", matches: git::is_log, apply: git::filter_log },
    Filter { name: "git-status", matches: git::is_status, apply: git::filter_status },
    Filter { name: "cargo", matches: cargo::matches, apply: cargo::filter },
    Filter { name: "python", matches: python::matches, apply: python::filter },
    Filter { name: "test-runner", matches: tests::matches, apply: tests::filter },
    Filter { name: "package-manager", matches: system::is_package_manager, apply: system::filter_package_manager },
    Filter { name: "file-list", matches: system::is_file_listing, apply: system::filter_file_listing },
    Filter { name: "environment", matches: system::is_environment, apply: system::filter_environment },
    Filter { name: "json", matches: system::is_json_command, apply: system::filter_json },
    Filter { name: "logs", matches: system::is_log_command, apply: system::filter_logs },
    Filter { name: "default", matches: |_| true, apply: default::filter },
];

pub fn process(command: &str, raw: &str) -> (String, &'static str) {
    for filter in FILTERS {
        if (filter.matches)(command) {
            return ((filter.apply)(raw), filter.name);
        }
    }
    unreachable!("the default output filter must match every command")
}

/// Recognized command patterns for reducer dispatch.
///
/// Used by the display pipeline to select the appropriate reducer.
/// `Generic` means no specialized reducer is registered for this command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedCommand {
    /// `pytest` invoked directly, or `python* -m pytest`
    Pytest { module_invocation: bool },
    /// `cargo test [...]`
    CargoTest,
    /// Everything else — falls through to generic head/tail condenser
    Generic,
}

/// Normalize `command` + `args` into a `NormalizedCommand`.
///
/// Rules (evaluated in priority order):
/// 1. `cargo test …` → `CargoTest`
/// 2. `pytest` (basename) → `Pytest { module_invocation: false }`
/// 3. `python` / `python3` / `python3.12` + adjacent `-m pytest` → `Pytest { module_invocation: true }`
/// 4. Anything else → `Generic`
pub fn normalize(command: &str, args: &[String]) -> NormalizedCommand {
    let basename = command_basename(command);

    // Priority 1: cargo test
    if basename == "cargo" && args.first().map(|s| s.as_str()) == Some("test") {
        return NormalizedCommand::CargoTest;
    }

    // Priority 2: pytest invoked directly
    if basename == "pytest" {
        return NormalizedCommand::Pytest {
            module_invocation: false,
        };
    }

    // Priority 3: python* -m pytest (adjacent -m pytest required)
    if is_python_executable(basename) {
        if let Some(idx) = args.iter().position(|a| a == "-m") {
            if args.get(idx + 1).map(|s| s.as_str()) == Some("pytest") {
                return NormalizedCommand::Pytest {
                    module_invocation: true,
                };
            }
        }
    }

    NormalizedCommand::Generic
}

/// Extract the basename of `command` (last path component, no extension).
fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

/// Return `true` iff `basename` is a recognized Python executable.
///
/// Accepts: `python`, `python3`, `python3.12`, `python3.12.1`.
/// Rejects: `python-malware`, `pythonista`, `python_script`.
fn is_python_executable(basename: &str) -> bool {
    if basename == "python" {
        return true;
    }
    if let Some(suffix) = basename.strip_prefix("python") {
        if suffix.is_empty() {
            return true;
        }
        // suffix must start with a digit and contain only digits and dots
        let mut chars = suffix.chars();
        if chars.next().map(|c| c.is_ascii_digit()) == Some(true) {
            return suffix.chars().all(|c| c.is_ascii_digit() || c == '.');
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|&a| a.to_string()).collect()
    }

    // ── pytest direct ─────────────────────────────────────────────────────────

    #[test]
    fn test_pytest_direct() {
        assert_eq!(
            normalize("pytest", &[]),
            NormalizedCommand::Pytest {
                module_invocation: false
            }
        );
    }

    #[test]
    fn test_pytest_absolute_path() {
        assert_eq!(
            normalize("/usr/bin/pytest", &[]),
            NormalizedCommand::Pytest {
                module_invocation: false
            }
        );
    }

    #[test]
    fn test_pytest_with_args() {
        assert_eq!(
            normalize("pytest", &sv(&["-v", "tests/"])),
            NormalizedCommand::Pytest {
                module_invocation: false
            }
        );
    }

    // ── python -m pytest ──────────────────────────────────────────────────────

    #[test]
    fn test_python3_m_pytest() {
        assert_eq!(
            normalize("python3", &sv(&["-m", "pytest"])),
            NormalizedCommand::Pytest {
                module_invocation: true
            }
        );
    }

    #[test]
    fn test_python_m_pytest() {
        assert_eq!(
            normalize("python", &sv(&["-m", "pytest"])),
            NormalizedCommand::Pytest {
                module_invocation: true
            }
        );
    }

    #[test]
    fn test_python3_12_m_pytest() {
        assert_eq!(
            normalize("python3.12", &sv(&["-m", "pytest"])),
            NormalizedCommand::Pytest {
                module_invocation: true
            }
        );
    }

    #[test]
    fn test_python_m_pytest_with_extra_args() {
        assert_eq!(
            normalize("python3", &sv(&["-m", "pytest", "-v", "tests/"])),
            NormalizedCommand::Pytest {
                module_invocation: true
            }
        );
    }

    #[test]
    fn test_python_absolute_path_m_pytest() {
        assert_eq!(
            normalize("/usr/bin/python3", &sv(&["-m", "pytest"])),
            NormalizedCommand::Pytest {
                module_invocation: true
            }
        );
    }

    // ── NOT pytest — python -m something_else ────────────────────────────────

    #[test]
    fn test_python_m_unittest_is_generic() {
        assert_eq!(
            normalize("python3", &sv(&["-m", "unittest"])),
            NormalizedCommand::Generic
        );
    }

    #[test]
    fn test_python_no_m_flag_is_generic() {
        assert_eq!(
            normalize("python3", &sv(&["script.py"])),
            NormalizedCommand::Generic
        );
    }

    /// `-m` not immediately followed by `pytest` → Generic
    #[test]
    fn test_python_m_not_adjacent_pytest_is_generic() {
        assert_eq!(
            normalize("python3", &sv(&["-m", "coverage", "run", "pytest"])),
            NormalizedCommand::Generic
        );
    }

    /// `python-malware` must NOT match python* pattern
    #[test]
    fn test_python_malware_is_generic() {
        assert_eq!(
            normalize("python-malware", &sv(&["-m", "pytest"])),
            NormalizedCommand::Generic
        );
    }

    // ── cargo test ────────────────────────────────────────────────────────────

    #[test]
    fn test_cargo_test() {
        assert_eq!(
            normalize("cargo", &sv(&["test"])),
            NormalizedCommand::CargoTest
        );
    }

    #[test]
    fn test_cargo_test_with_flags() {
        assert_eq!(
            normalize("cargo", &sv(&["test", "--lib"])),
            NormalizedCommand::CargoTest
        );
    }

    #[test]
    fn test_cargo_absolute_path_test() {
        assert_eq!(
            normalize("/usr/bin/cargo", &sv(&["test"])),
            NormalizedCommand::CargoTest
        );
    }

    /// `cargo build` is Generic (first arg is not `test`)
    #[test]
    fn test_cargo_build_is_generic() {
        assert_eq!(
            normalize("cargo", &sv(&["build"])),
            NormalizedCommand::Generic
        );
    }

    /// `cargo` with no args is Generic
    #[test]
    fn test_cargo_no_args_is_generic() {
        assert_eq!(normalize("cargo", &[]), NormalizedCommand::Generic);
    }

    // ── generic fallback ──────────────────────────────────────────────────────

    #[test]
    fn test_git_diff_is_generic() {
        assert_eq!(normalize("git", &sv(&["diff"])), NormalizedCommand::Generic);
    }

    #[test]
    fn test_seq_is_generic() {
        assert_eq!(
            normalize("seq", &sv(&["1", "200"])),
            NormalizedCommand::Generic
        );
    }

    // ── is_python_executable helper ───────────────────────────────────────────

    #[test]
    fn test_is_python_executable_variants() {
        assert!(is_python_executable("python"));
        assert!(is_python_executable("python3"));
        assert!(is_python_executable("python3.12"));
        assert!(is_python_executable("python3.12.1"));
        assert!(!is_python_executable("python-malware"));
        assert!(!is_python_executable("pythonista"));
        assert!(!is_python_executable("python_script"));
        assert!(!is_python_executable("cpython"));
    }
}

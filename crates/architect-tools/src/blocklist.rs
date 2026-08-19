//! A best-effort denylist for `run_command`.
//!
//! Not a sandbox. There is no seccomp, no container, no chroot — this is a
//! deny-list checked against the tokenized command string, and it is checked
//! against *tokens*, not shell semantics. A quoted or obfuscated command
//! bypasses it by design: `bash -c "printf 'rm -rf /'"` runs. It stops the
//! obvious, unintentional mistake, not a deliberate attempt to get around it.
//! Ported from V1's `src/tools/blocklist.rs`, which took the same position.

use std::sync::LazyLock;

use regex::Regex;

/// Bare command names that are never allowed, regardless of arguments.
const BLOCKED_COMMANDS: &[&str] = &[
    "dd", "mkfs", "format", "shutdown", "reboot", "poweroff", "halt", "kill", "pkill", "chown",
    "passwd", "wget", "rmdir", "del",
];

/// `(command, first_argument)` pairs that are always blocked.
const BLOCKED_COMMAND_ARGS: &[(&str, &str)] = &[
    ("rm", "-rf"),
    ("rm", "-fr"),
    ("rm", "--recursive"),
    ("chmod", "0"),
    ("chmod", "777"),
];

/// Checked against the first two whitespace-separated tokens of each
/// statement.
static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^rm\s+-[rfR]",
        r"^chmod\s+0",
        r"^chmod\s+777",
        r"^curl\s+-o\s+/",
        r"^mv\s+.*\s+/dev/null",
        r"^:\(\)\s*\{",
        r">\s*/dev/sd",
        r">\s*/dev/nvme",
        r">\s*/dev/disk",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("blocklist pattern is valid"))
    .collect()
});

/// Reject a command, with the reason it was rejected.
pub fn check(command: &str) -> Result<(), String> {
    for statement in split_statements(command) {
        check_statement(statement.trim())?;
    }
    Ok(())
}

/// Split on shell statement separators. Not quote-aware — see the module doc:
/// that is a known, accepted limitation, not a bug to fix here.
fn split_statements(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(['\n', ';'])
        .flat_map(|s| s.split("&&"))
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split('|'))
}

fn check_statement(statement: &str) -> Result<(), String> {
    if statement.is_empty() {
        return Ok(());
    }

    let mut tokens = statement.split_whitespace();
    let Some(command) = tokens.next() else {
        return Ok(());
    };
    let command = command.rsplit('/').next().unwrap_or(command);

    if command == "su" || command == "sudo" {
        return Err(format!("{command:?} is never allowed"));
    }

    if BLOCKED_COMMANDS.contains(&command) {
        return Err(format!("{command:?} is not allowed"));
    }

    if let Some(first_arg) = tokens.next()
        && BLOCKED_COMMAND_ARGS.contains(&(command, first_arg))
    {
        return Err(format!("{command} {first_arg} is not allowed"));
    }

    for pattern in PATTERNS.iter() {
        if pattern.is_match(statement) {
            return Err(format!("{statement:?} matches a blocked pattern"));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_recursive_rm() {
        assert!(check("rm -rf /home/user/project").is_err());
        assert!(check("rm -rf .").is_err());
    }

    #[test]
    fn blocks_su_and_sudo_unconditionally() {
        assert!(check("sudo apt install foo").is_err());
        assert!(check("su root").is_err());
    }

    #[test]
    fn blocks_a_fork_bomb() {
        assert!(check(":(){ :|:& };:").is_err());
    }

    #[test]
    fn blocks_within_a_compound_statement() {
        assert!(check("echo hi && rm -rf /").is_err());
        assert!(check("ls; sudo reboot").is_err());
    }

    #[test]
    fn allows_ordinary_commands() {
        assert!(check("cargo test --workspace").is_ok());
        assert!(check("git status").is_ok());
        assert!(check("rm file.txt").is_ok(), "a non-recursive rm is fine");
    }

    #[test]
    fn a_quoted_command_is_not_caught_by_design() {
        // Documented limitation, not a bug: this is a denylist over tokens,
        // not a shell parser.
        assert!(check("bash -c \"rm -rf /\"").is_ok());
    }
}

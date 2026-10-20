use std::process::Command;

use crate::proc;

#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub repository: String,
    pub name: String,
    pub version: String,
}

/// Find the current version of `package` in the *official* repositories.
///
/// The local repository is deliberately skipped: it has top priority, so
/// asking pacman for a plain `package` would return the version pot built
/// last time instead of the current upstream version.
pub fn resolve(package: &str, local_repository: &str) -> Result<Resolved, String> {
    let repositories = sync_repositories(local_repository)?;

    let mut last_error = String::new();

    for repository in &repositories {
        let target = format!("{repository}/{package}");

        let result = proc::capture(Command::new("pacman").args([
            "-Sp",
            "--print-format",
            "%r %n %v",
            &target,
        ]))
        .map_err(|error| error.to_string())?;

        if result.success {
            if let Some(resolved) = parse_print_format(&result.stdout, package) {
                return Ok(resolved);
            }
        } else {
            last_error = result.error_line();
        }
    }

    if repositories.is_empty() {
        return Err("pacman-conf lists no repositories besides the local one".to_owned());
    }

    Err(if last_error.is_empty() {
        format!("package '{package}' was not found in the sync repositories")
    } else {
        format!("package '{package}' was not found in the sync repositories ({last_error})")
    })
}

/// Resolve a package from one specifically named repository.
fn resolve_from_repository(package: &str, repository: &str) -> Result<Resolved, String> {
    let target = format!("{repository}/{package}");
    let result = proc::capture(Command::new("pacman").args([
        "-Sp",
        "--print-format",
        "%r %n %v",
        &target,
    ]))
    .map_err(|error| error.to_string())?;

    if !result.success {
        return Err(format!(
            "cannot resolve {target}: {}",
            result.error_line()
        ));
    }

    parse_print_format(&result.stdout, package)
        .ok_or_else(|| format!("repository '{repository}' returned no version for '{package}'"))
}

fn sync_repositories(local_repository: &str) -> Result<Vec<String>, String> {
    let result = proc::capture(Command::new("pacman-conf").arg("--repo-list"))
        .map_err(|error| error.to_string())?;

    if !result.success {
        return Err(format!(
            "pacman-conf --repo-list failed: {}",
            result.error_line()
        ));
    }

    Ok(result
        .stdout
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != local_repository)
        .map(str::to_owned)
        .collect())
}

/// `pacman -Sp` prints dependencies that are not installed *before* the
/// requested package, so pick the line that belongs to the package itself.
fn parse_print_format(output: &str, package: &str) -> Option<Resolved> {
    output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();

        let repository = fields.next()?;
        let name = fields.next()?;
        let version = fields.next()?;

        (name == package).then(|| Resolved {
            repository: repository.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
        })
    })
}

/// Classify a cached version against the official repository version.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VersionStatus {
    UpdateAvailable,
    Current,
    CachedNewer,
}

fn classify_version_comparison(comparison: i32) -> VersionStatus {
    match comparison.cmp(&0) {
        std::cmp::Ordering::Less => VersionStatus::UpdateAvailable,
        std::cmp::Ordering::Equal => VersionStatus::Current,
        std::cmp::Ordering::Greater => VersionStatus::CachedNewer,
    }
}

/// Compare the package in POT's local repository with the official repositories.
pub(crate) fn check_repository_version(
    package_name: &str,
    local_repository: &str,
) -> Result<VersionStatus, String> {
    let local = resolve_from_repository(package_name, local_repository)?;
    let official = resolve(package_name, local_repository)?;

    let comparison = compare_versions(&local.version, &official.version).map_err(|error| {
        format!(
            "cannot compare local version {} with official version {}: {error}",
            local.version, official.version
        )
    })?;

    Ok(classify_version_comparison(comparison))
}

/// Compare two Arch package versions with `vercmp`.
///
/// The result is negative when `cached` is older, zero when equal,
/// and positive when `cached` is newer than `official`.
fn compare_versions(cached: &str, official: &str) -> Result<i32, String> {
    let result = proc::capture(Command::new("vercmp").args([cached, official]))
        .map_err(|error| error.to_string())?;

    parse_vercmp_result(&result)
}

fn parse_vercmp_result(result: &proc::Captured) -> Result<i32, String> {
    if !result.success {
        return Err(format!("vercmp failed: {}", result.error_line()));
    }

    let lines: Vec<&str> = result.stdout.lines().collect();
    if lines.len() != 1 || lines[0].trim().is_empty() {
        return Err(format!(
            "vercmp returned unexpected output: {:?}",
            result.stdout
        ));
    }

    let output = lines[0].trim();
    output
        .parse::<i32>()
        .map_err(|_| format!("vercmp returned invalid output: {output:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_requested_package_not_its_dependencies() {
        let output = "\
extra libfoo 1.2-1
core glib2 2.80.0-1
extra nautilus 1:47.1-2
";

        assert_eq!(
            parse_print_format(output, "nautilus"),
            Some(Resolved {
                repository: "extra".to_owned(),
                name: "nautilus".to_owned(),
                version: "1:47.1-2".to_owned(),
            })
        );
    }

    #[test]
    fn classifies_version_comparison() {
        assert_eq!(
            classify_version_comparison(-1),
            VersionStatus::UpdateAvailable
        );
        assert_eq!(classify_version_comparison(0), VersionStatus::Current);
        assert_eq!(classify_version_comparison(1), VersionStatus::CachedNewer);
        assert_eq!(
            classify_version_comparison(-42),
            VersionStatus::UpdateAvailable
        );
        assert_eq!(classify_version_comparison(42), VersionStatus::CachedNewer);
    }

    fn captured(success: bool, stdout: &str, stderr: &str) -> proc::Captured {
        proc::Captured {
            success,
            code: Some(if success { 0 } else { 1 }),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn parses_valid_vercmp_results() {
        assert_eq!(parse_vercmp_result(&captured(true, "-1\n", "")), Ok(-1));
        assert_eq!(parse_vercmp_result(&captured(true, "0\n", "")), Ok(0));
        assert_eq!(parse_vercmp_result(&captured(true, "1\n", "")), Ok(1));
    }

    #[test]
    fn rejects_unexpected_vercmp_output() {
        for output in ["", "\n", "0\n1\n"] {
            assert!(
                parse_vercmp_result(&captured(true, output, "")).is_err(),
                "should reject output {output:?}"
            );
        }

        let error = parse_vercmp_result(&captured(true, "not-a-number\n", "")).unwrap_err();
        assert!(error.contains("invalid output"), "{error}");
    }

    #[test]
    fn reports_vercmp_command_failure() {
        let error = parse_vercmp_result(&captured(false, "", "invalid version\n")).unwrap_err();

        assert!(error.contains("vercmp failed"), "{error}");
        assert!(error.contains("invalid version"), "{error}");
    }

    #[test]
    fn missing_package_is_none() {
        assert_eq!(parse_print_format("extra other 1-1\n", "nautilus"), None);
        assert_eq!(parse_print_format("", "nautilus"), None);
    }
}

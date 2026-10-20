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

/// Read the package version fields from `.SRCINFO`.
///
/// The returned version is formatted for Arch's `vercmp`:
/// `epoch:pkgver-pkgrel`.
fn parse_srcinfo_version(srcinfo: &str) -> Result<String, String> {
    let mut epoch: Option<&str> = None;
    let mut pkgver: Option<&str> = None;
    let mut pkgrel: Option<&str> = None;

    for line in srcinfo.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();

        let slot = match key {
            "epoch" => &mut epoch,
            "pkgver" => &mut pkgver,
            "pkgrel" => &mut pkgrel,
            _ => continue,
        };

        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(format!("invalid empty or whitespace-containing {key}"));
        }

        if slot.replace(value).is_some() {
            return Err(format!("duplicate {key} field"));
        }
    }

    let pkgver = pkgver.ok_or_else(|| "missing pkgver field".to_owned())?;
    let pkgrel = pkgrel.ok_or_else(|| "missing pkgrel field".to_owned())?;
    let epoch = epoch.unwrap_or("0");

    Ok(format!("{epoch}:{pkgver}-{pkgrel}"))
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

/// Compare a cached recipe's `.SRCINFO` version with the official repository.
///
/// This is read-only: it does not alter the cached recipe or its sources.
pub(crate) fn check_cached_version(
    srcinfo_path: &std::path::Path,
    package_name: &str,
    local_repository: &str,
) -> Result<VersionStatus, String> {
    let work_dir = srcinfo_path.parent().ok_or_else(|| {
        format!(
            "cannot determine recipe directory from {}",
            srcinfo_path.display()
        )
    })?;
    let pkgbuild_path = work_dir.join("PKGBUILD");

    if !pkgbuild_path.is_file() {
        return Err(format!(
            "cannot read {}: file does not exist",
            pkgbuild_path.display()
        ));
    }

    let generated = proc::capture(
        Command::new("makepkg")
            .arg("--printsrcinfo")
            .current_dir(work_dir),
    )
    .map_err(|error| format!("makepkg --printsrcinfo failed: {error}"))?;

    if !generated.success {
        return Err(format!(
            "makepkg --printsrcinfo failed: {}",
            generated.error_line()
        ));
    }

    let cached = parse_srcinfo_version(&generated.stdout).map_err(|error| {
        format!(
            "invalid generated SRCINFO for {}: {error}",
            work_dir.display()
        )
    })?;

    let official = resolve(package_name, local_repository)?;

    let comparison = compare_versions(&cached, &official.version).map_err(|error| {
        format!(
            "cannot compare cached version {cached} with official version {}: {error}",
            official.version
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
    fn parses_srcinfo_version_without_epoch() {
        let srcinfo = "pkgbase = example\npkgver = 2.4.1\npkgrel = 3\n";

        assert_eq!(parse_srcinfo_version(srcinfo), Ok("0:2.4.1-3".to_owned()));
    }

    #[test]
    fn parses_srcinfo_version_with_epoch_and_ignores_other_fields() {
        let srcinfo = "# generated file\npkgbase = example\nepoch = 2\npkgver = 1.0\npkgrel = 1\narch = any\n";

        assert_eq!(parse_srcinfo_version(srcinfo), Ok("2:1.0-1".to_owned()));
    }

    #[test]
    fn rejects_missing_empty_and_duplicate_version_fields() {
        assert!(parse_srcinfo_version("pkgrel = 1\n").is_err());
        assert!(parse_srcinfo_version("pkgver = 1.0\n").is_err());
        assert!(parse_srcinfo_version("pkgver =\npkgrel = 1\n").is_err());
        assert!(parse_srcinfo_version("pkgver = 1.0\npkgver = 2.0\npkgrel = 1\n").is_err());
        assert!(parse_srcinfo_version("pkgver = 1.0\npkgrel = 1\npkgrel = 2\n").is_err());
        assert!(parse_srcinfo_version("epoch = 1\nepoch = 2\npkgver = 1.0\npkgrel = 1\n").is_err());
    }

    #[test]
    fn check_cached_version_reports_missing_srcinfo() {
        let dir = std::env::temp_dir().join(format!(
            "pot-check-cached-version-missing-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let srcinfo_path = dir.join(".SRCINFO");
        let _ = std::fs::remove_file(&srcinfo_path);

        let result = check_cached_version(&srcinfo_path, "example", "pot");

        let _ = std::fs::remove_dir_all(&dir);

        let error = result.unwrap_err();
        assert!(error.contains("cannot read"), "{error}");
        assert!(error.contains("PKGBUILD"), "{error}");
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

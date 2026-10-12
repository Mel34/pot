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
        return Err(format!("pacman-conf --repo-list failed: {}", result.error_line()));
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
    fn missing_package_is_none() {
        assert_eq!(parse_print_format("extra other 1-1\n", "nautilus"), None);
        assert_eq!(parse_print_format("", "nautilus"), None);
    }
}

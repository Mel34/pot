//! Fetching a package's PKGBUILD and preparing its sources.
//!
//! Every step reports what it is doing, streams the output of the tools it
//! runs, and fails with a message that says which step failed and why.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::overlay;
use crate::package;
use crate::proc;
use crate::worker::{Env, JobError, Reporter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepState {
    /// Nothing on disk.
    Missing,
    /// A previous attempt did not finish; it will be redone from scratch.
    Incomplete,
    /// Sources are in place; the recipe can be cooked.
    Ready,
}

pub fn state(cache_dir: &Path, recipe: &str) -> PrepState {
    let work_root = cache_dir.join("work");
    let work_dir = work_root.join(recipe);

    if marker_path(&work_root, recipe).exists() {
        return PrepState::Incomplete;
    }

    if !work_dir.exists() {
        return PrepState::Missing;
    }

    if work_dir.join("src").is_dir() {
        PrepState::Ready
    } else {
        PrepState::Incomplete
    }
}

/// Exists while a preparation is under way (or died half way).
fn marker_path(work_root: &Path, recipe: &str) -> PathBuf {
    work_root.join(format!(".{recipe}.incomplete"))
}

/// A `git+` entry of the PKGBUILD's `source` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// Directory name below `src/` (the `name::` prefix, else the repository).
    pub name: String,
    pub url: String,
    /// `("tag", "v1.2")`, `("commit", "<sha>")`, `("branch", "main")`, ...
    pub reference: Option<(String, String)>,
}

/// Extract the git sources from `makepkg --printsrcinfo` output.
///
/// Understands `name::git+url`, architecture specific `source_x86_64 = ...`
/// entries and the `?signed` query that can follow the fragment.
pub fn parse_git_sources(srcinfo: &str) -> Vec<GitSource> {
    let mut sources = Vec::new();

    for line in srcinfo.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();

        if key != "source" && !key.starts_with("source_") {
            continue;
        }

        let value = value.trim().trim_matches('"');

        let (explicit_name, location) = match value.split_once("::") {
            Some((name, rest)) => (Some(name), rest),
            None => (None, value),
        };

        let Some(location) = location.strip_prefix("git+") else {
            continue;
        };

        let (url, fragment) = match location.split_once('#') {
            Some((url, fragment)) => (url, Some(fragment)),
            None => (location, None),
        };

        let url = url.split('?').next().unwrap_or(url);

        let reference = fragment
            .map(|fragment| fragment.split('?').next().unwrap_or(fragment))
            .and_then(|fragment| fragment.split_once('='))
            .map(|(kind, value)| (kind.to_owned(), value.to_owned()));

        let name = explicit_name.map(str::to_owned).unwrap_or_else(|| {
            let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
            last.strip_suffix(".git").unwrap_or(last).to_owned()
        });

        sources.push(GitSource {
            name,
            url: url.to_owned(),
            reference,
        });
    }

    sources
}

pub fn load_git_sources(work_dir: &Path) -> Result<Vec<GitSource>, String> {
    let result = proc::capture(
        Command::new("makepkg")
            .arg("--printsrcinfo")
            .current_dir(work_dir),
    )
    .map_err(|error| error.to_string())?;

    if !result.success {
        return Err(format!(
            "makepkg --printsrcinfo failed: {}",
            result.error_line()
        ));
    }

    Ok(parse_git_sources(&result.stdout))
}

fn check_cancelled(cancel: &dyn Fn() -> bool) -> Result<(), JobError> {
    if cancel() {
        Err(JobError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn prepare_sources(
    env: &Env,
    recipe: &str,
    reset: bool,
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
) -> Result<(), JobError> {
    let work_root = env.work_root();
    let work_dir = env.work_dir(recipe);
    let marker = marker_path(&work_root, recipe);

    let merged = env.overlay_root(recipe).join("merged");

    if overlay::is_mounted(&merged) {
        return Err(JobError::Failed(format!(
            "an overlay is still mounted at {}; leave the cook view first",
            merged.display()
        )));
    }

    rep.phase("resolving the current package version");

    let resolved = package::resolve(recipe, &env.repository_name)?;

    rep.note(&format!(
        "{} {} from the [{}] repository",
        resolved.name, resolved.version, resolved.repository
    ));

    check_cancelled(cancel)?;

    if work_dir.exists() {
        rep.phase(if reset {
            "removing the old source tree"
        } else {
            "discarding the incomplete source tree from an earlier attempt"
        });

        fs::remove_dir_all(&work_dir)
            .map_err(|error| format!("cannot remove {}: {error}", work_dir.display()))?;
    }

    fs::create_dir_all(&work_root)
        .map_err(|error| format!("cannot create {}: {error}", work_root.display()))?;

    // Until this is removed again the tree does not count as prepared, so an
    // interrupted attempt can never show up as "ready to cook".
    fs::write(&marker, &resolved.version)
        .map_err(|error| format!("cannot write {}: {error}", marker.display()))?;

    env.forget_sources(&work_dir);

    rep.phase("cloning the PKGBUILD repository (pkgctl)");

    rep.run(
        "pkgctl repo clone",
        Command::new("pkgctl")
            .args(["repo", "clone", "--protocol", "https", "--switch"])
            .arg(&resolved.version)
            .arg(recipe)
            .current_dir(&work_root),
        cancel,
    )
    .map_err(|error| hint_missing(error, "pkgctl", "devtools"))?;

    check_cancelled(cancel)?;

    rep.phase("reading the source list (.SRCINFO)");

    let git_sources = load_git_sources(&work_dir)?;

    if git_sources.is_empty() {
        rep.note("no git+ sources found; patches cannot be applied to this package");
    }

    for source in &git_sources {
        check_cancelled(cancel)?;
        prefetch_git_source(rep, cancel, &env.cache_dir, &work_dir, source)?;
    }

    check_cancelled(cancel)?;

    rep.phase("downloading and extracting sources (makepkg -o)");

    rep.run(
        "makepkg",
        Command::new("makepkg")
            .args(["-sfo", "--noconfirm", "--holdver"])
            .current_dir(&work_dir),
        cancel,
    )?;

    fs::remove_file(&marker)
        .map_err(|error| format!("cannot remove {}: {error}", marker.display()))?;

    Ok(())
}

fn hint_missing(error: JobError, _program: &str, package: &str) -> JobError {
    match error {
        JobError::Failed(message) if message.contains("was not found in PATH") => {
            JobError::Failed(format!("{message} (install the '{package}' package)"))
        }
        other => other,
    }
}

/// Shallow-fetch a tag or commit into the layout makepkg expects, so makepkg
/// does not clone the whole history.
///
/// This is an optimisation: when it cannot be done (branch sources, servers
/// that refuse fetching by commit, ...) the partial result is removed and
/// makepkg fetches the source itself.
fn prefetch_git_source(
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
    cache_dir: &Path,
    work_dir: &Path,
    source: &GitSource,
) -> Result<(), JobError> {
    let Some((kind, revision)) = &source.reference else {
        rep.note(&format!(
            "{}: no tag or commit given; makepkg will fetch it",
            source.name
        ));
        return Ok(());
    };

    if kind != "tag" && kind != "commit" {
        rep.note(&format!(
            "{}: {kind} sources are fetched by makepkg",
            source.name
        ));
        return Ok(());
    }

    let cache_repo = cache_dir.join("git").join(&source.name);
    let checkout = work_dir.join("src").join(&source.name);

    if checkout.exists() {
        return Ok(());
    }

    // A persistent cache must never be reused for a different repository.
    // Treat identity problems as fatal instead of silently falling back.
    if cache_repo.exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(&cache_repo)
            .args(["remote", "get-url", "origin"])
            .output()
            .map_err(|error| {
                JobError::Failed(format!(
                    "cannot inspect Git cache {}: {error}",
                    cache_repo.display()
                ))
            })?;

        if !output.status.success() {
            return Err(JobError::Failed(format!(
                "Git cache {} has no readable origin; inspect or remove it manually",
                cache_repo.display()
            )));
        }

        let actual_url = String::from_utf8_lossy(&output.stdout).trim().to_owned();

        if actual_url != source.url {
            return Err(JobError::Failed(format!(
                "Git cache identity mismatch for {}: expected {}, found {}; refusing to reuse it",
                source.name, source.url, actual_url
            )));
        }
    }

    rep.phase(&format!("fetching {} {revision} (shallow)", source.name));

    let result = shallow_fetch(rep, cancel, &cache_repo, &checkout, source, kind, revision);

    match result {
        Ok(()) => Ok(()),

        Err(JobError::Cancelled) => Err(JobError::Cancelled),

        Err(JobError::Failed(message)) => {
            rep.note(&format!(
                "shallow fetch of {} failed ({message}); makepkg will fetch it instead",
                source.name
            ));

            // The bare cache is persistent: a failed fetch must not destroy
            // previously downloaded Git objects. Only discard the checkout.
            let _ = fs::remove_dir_all(&checkout);

            Ok(())
        }
    }
}

fn shallow_fetch(
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
    cache_repo: &Path,
    checkout: &Path,
    source: &GitSource,
    kind: &str,
    revision: &str,
) -> Result<(), JobError> {
    if !cache_repo.exists() {
        if let Some(parent) = cache_repo.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }

        rep.run(
            "git init",
            Command::new("git").args(["init", "--bare"]).arg(cache_repo),
            cancel,
        )?;

        rep.run(
            "git remote add",
            Command::new("git").arg("-C").arg(cache_repo).args([
                "remote",
                "add",
                "origin",
                &source.url,
            ]),
            cancel,
        )?;
    }

    let refspec = if kind == "tag" {
        format!("refs/tags/{revision}:refs/tags/{revision}")
    } else {
        // A commit is not a ref name; fetch it into a branch of our own.
        format!("{revision}:refs/heads/pot-{revision}")
    };

    rep.run(
        "git fetch",
        Command::new("git").arg("-C").arg(cache_repo).args([
            "fetch",
            "--depth=1",
            "origin",
            &refspec,
        ]),
        cancel,
    )?;

    rep.run(
        "git clone",
        Command::new("git")
            .args(["clone", "--no-checkout"])
            .arg(cache_repo)
            .arg(checkout),
        cancel,
    )?;

    rep.run(
        "git checkout",
        Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["checkout", "--detach", revision]),
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(kind: &str, value: &str) -> Option<(String, String)> {
        Some((kind.to_owned(), value.to_owned()))
    }

    #[test]
    fn parses_plain_git_sources() {
        let srcinfo = "\
pkgbase = nautilus
\tpkgver = 47.1
\tsource = git+https://gitlab.gnome.org/GNOME/nautilus.git#tag=47.1
\tsource = fix.patch
";

        assert_eq!(
            parse_git_sources(srcinfo),
            vec![GitSource {
                name: "nautilus".to_owned(),
                url: "https://gitlab.gnome.org/GNOME/nautilus.git".to_owned(),
                reference: reference("tag", "47.1"),
            }]
        );
    }

    #[test]
    fn parses_named_arch_specific_and_signed_sources() {
        let srcinfo = "\
\tsource = foo::git+https://example.org/x/foo-upstream.git#commit=abc123?signed
\tsource_x86_64 = git+https://example.org/bar.git?signed#tag=v2.0?signed
\tsource = git+https://example.org/baz
\tsource = https://example.org/not-git.tar.gz
";

        assert_eq!(
            parse_git_sources(srcinfo),
            vec![
                GitSource {
                    name: "foo".to_owned(),
                    url: "https://example.org/x/foo-upstream.git".to_owned(),
                    reference: reference("commit", "abc123"),
                },
                GitSource {
                    name: "bar".to_owned(),
                    url: "https://example.org/bar.git".to_owned(),
                    reference: reference("tag", "v2.0"),
                },
                GitSource {
                    name: "baz".to_owned(),
                    url: "https://example.org/baz".to_owned(),
                    reference: None,
                },
            ]
        );
    }

    #[test]
    fn state_follows_the_files_on_disk() {
        let cache = std::env::temp_dir().join(format!("pot-prepare-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);

        assert_eq!(state(&cache, "pkg"), PrepState::Missing);

        fs::create_dir_all(cache.join("work/pkg")).unwrap();
        assert_eq!(state(&cache, "pkg"), PrepState::Incomplete);

        fs::create_dir_all(cache.join("work/pkg/src")).unwrap();
        assert_eq!(state(&cache, "pkg"), PrepState::Ready);

        fs::write(cache.join("work/.pkg.incomplete"), "1.0-1").unwrap();
        assert_eq!(state(&cache, "pkg"), PrepState::Incomplete);

        fs::remove_dir_all(cache).unwrap();
    }
}

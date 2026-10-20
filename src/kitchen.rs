//! Cooking: mount an overlay over the prepared sources, apply the selected
//! patches, build with makepkg and publish the result to the local repository.
//!
//! Runs on the cook lane thread. Progress is reported through the [`Reporter`];
//! the interface lives in `ui.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::overlay::{self, Overlay};
use crate::patch::{self, Verification};
use crate::prepare::{self, PrepState};
use crate::repository;
use crate::worker::{CookReq, Env, JobError, Reporter};

fn check_cancelled(cancel: &dyn Fn() -> bool) -> Result<(), JobError> {
    if cancel() {
        Err(JobError::Cancelled)
    } else {
        Ok(())
    }
}

fn is_package(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".pkg.tar.zst"))
}

/// The mounted overlay is handed back through `overlay_slot` so the caller
/// still owns (and unmounts) it if this function fails or panics half way.
pub fn cook(
    env: &Env,
    request: &CookReq,
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
    overlay_slot: &mut Option<Overlay>,
) -> Result<(), JobError> {
    let recipe = request.recipe_name.as_str();
    let work_dir = env.work_dir(recipe);

    if prepare::state(&env.cache_dir, recipe) != PrepState::Ready {
        return Err("the sources are not prepared; prepare the recipe first".into());
    }

    let repository_dir = repository::repository_dir(&env.repository_name)?;

    // Stale packages in the source tree would be mistaken for fresh ones.
    // Done before mounting: the lower layer must not change underneath an
    // active overlay.
    remove_stale_packages(&work_dir, rep)?;

    check_cancelled(cancel)?;

    rep.phase("mounting the overlay");

    let overlay_root = env.overlay_root(recipe);
    let stale_mount = overlay_root.join("merged");

    if overlay::is_mounted(&stale_mount) {
        rep.note("an overlay from an earlier run is still mounted; unmounting it first");
        overlay::unmount_path(&stale_mount)?;
    }

    let overlay = Overlay::mount(&work_dir, &overlay_root)?;
    let merged = overlay.merged.clone();

    *overlay_slot = Some(overlay);

    rep.note(&format!("overlay mounted at {}", merged.display()));

    apply_patches(env, request, rep, cancel, &work_dir)?;

    check_cancelled(cancel)?;

    rep.phase("preparing patched sources and building (makepkg)");

    run_makepkg_with_prepare(&merged, rep, cancel)?;

    check_cancelled(cancel)?;

    rep.phase("copying packages into the local repository");

    let packages = publish_packages(&merged, &repository_dir, rep)?;

    rep.phase("updating the repository database (repo-add)");

    let database = repository_dir.join(format!("{}.db.tar.gz", env.repository_name));

    rep.run(
        "repo-add",
        Command::new("repo-add")
            .arg(&database)
            .args(packages.iter().map(|package| repository_dir.join(package))),
        cancel,
    )?;

    rep.phase("syncing the pacman database");

    rep.run(
        "syncing the pacman database",
        &mut crate::helper::sync_packages_command(&env.repository_name),
        cancel,
    )?;

    Ok(())
}

fn remove_stale_packages(work_dir: &Path, rep: &Reporter) -> Result<(), JobError> {
    let entries = fs::read_dir(work_dir)
        .map_err(|error| format!("cannot read {}: {error}", work_dir.display()))?;

    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", work_dir.display()))?
            .path();

        if is_package(&path) {
            rep.note(&format!("removing stale package {}", path.display()));

            fs::remove_file(&path).map_err(|error| {
                format!("cannot remove stale package {}: {error}", path.display())
            })?;
        }
    }

    Ok(())
}

fn run_makepkg_with_prepare(
    merged: &Path,
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
) -> Result<(), JobError> {
    let script_name = format!(".PKGBUILD.pot-{}", std::process::id());
    let script_path = merged.join(&script_name);

    let script = r#"source "$PWD/PKGBUILD"

if ! declare -F build >/dev/null; then
    printf '%s\n' 'POT: original PKGBUILD has no build() function' >&2
    return 1
fi

_pot_build_definition=$(declare -f build)
eval "${_pot_build_definition/#build/_pot_original_build}"
unset _pot_build_definition

build() {
    cd "$srcdir"
    _pot_original_build
}
"#;

    std::fs::write(&script_path, script)
        .map_err(|error| format!("cannot write {}: {error}", script_path.display()))?;

    let result = rep.run(
        "makepkg",
        Command::new("makepkg")
            .args(["-e", "--nocheck", "--holdver", "-p"])
            .arg(&script_name)
            .current_dir(merged),
        cancel,
    );

    let cleanup = std::fs::remove_file(&script_path);

    result?;
    cleanup.map_err(|error| format!("cannot remove {}: {error}", script_path.display()))?;

    Ok(())
}

fn apply_patches(
    env: &Env,
    request: &CookReq,
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
    work_dir: &Path,
) -> Result<(), JobError> {
    if request.patches.is_empty() {
        rep.note("no patches selected; building the unmodified sources");
        return Ok(());
    }

    let overlay_root = env.overlay_root(&request.recipe_name);
    let merged = overlay_root.join("merged");

    let git_sources = env.git_sources(work_dir)?;
    let total = request.patches.len();

    for (index, patch_path) in request.patches.iter().enumerate() {
        check_cancelled(cancel)?;

        let name = patch_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("patch");

        rep.phase(&format!("applying {name} ({}/{total})", index + 1));

        let (source_dir, strip_level) = match patch::verify(&merged, &git_sources, patch_path) {
            Verification::Applicable(dir, strip) => (dir, strip),

            Verification::Applied(..) => {
                rep.note(&format!("{name} is already applied; skipping"));
                continue;
            }

            Verification::Conflict(error) => {
                for line in error.lines() {
                    rep.line(line);
                }

                return Err(JobError::Failed(format!(
                    "{name} does not apply to the sources"
                )));
            }
        };

        rep.note(&format!(
            "patching {} with -p{strip_level}",
            source_dir.display()
        ));

        rep.run(
            "git apply",
            Command::new("git")
                .arg("apply")
                .arg(format!("-p{strip_level}"))
                .arg(patch_path)
                .current_dir(&source_dir),
            cancel,
        )?;
    }

    Ok(())
}

/// Move the built packages (and their signatures) into the repository.
fn publish_packages(
    merged: &Path,
    repository_dir: &Path,
    rep: &Reporter,
) -> Result<Vec<PathBuf>, JobError> {
    let entries = fs::read_dir(merged)
        .map_err(|error| format!("cannot read {}: {error}", merged.display()))?;

    let mut packages = Vec::new();

    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", merged.display()))?
            .path();

        if !is_package(&path) {
            continue;
        }

        let file_name = path.file_name().map(PathBuf::from).unwrap_or_default();

        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .map_err(|error| format!("cannot set permissions on {}: {error}", path.display()))?;

        let destination = repository_dir.join(&file_name);

        fs::copy(&path, &destination).map_err(|error| {
            format!(
                "cannot copy {} into {}: {error}",
                file_name.display(),
                repository_dir.display()
            )
        })?;

        fs::remove_file(&path).map_err(|error| {
            format!("cannot remove {} from the overlay: {error}", path.display())
        })?;

        // A detached signature has to travel with its package.
        let signature = path.with_file_name(format!("{}.sig", file_name.to_string_lossy()));

        if signature.is_file() {
            fs::copy(
                &signature,
                repository_dir.join(format!("{}.sig", file_name.to_string_lossy())),
            )
            .map_err(|error| format!("cannot copy {}: {error}", signature.display()))?;
        }

        rep.note(&format!("published {}", file_name.display()));

        packages.push(file_name);
    }

    if packages.is_empty() {
        return Err("makepkg produced no package files".into());
    }

    Ok(packages)
}

pub fn install(
    package_name: &str,
    rep: &Reporter,
    cancel: &dyn Fn() -> bool,
) -> Result<(), JobError> {
    rep.phase("installing package");

    rep.run(
        "pacman -S",
        &mut crate::helper::install_package_command(package_name),
        cancel,
    )?;

    Ok(())
}

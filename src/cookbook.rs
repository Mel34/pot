use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct Recipe {
    pub name: String,
    pub path: PathBuf,
    pub patches: Vec<Patch>,
}

#[derive(Debug)]
pub struct Patch {
    pub path: PathBuf,
    pub selected: bool,
    pub status: PatchStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchStatus {
    Unknown,
    Clean,
    Failed,
}

impl Recipe {
    pub fn load_selection(&mut self) -> io::Result<()> {
        let state_path = self.path.join(".pot");

        if !state_path.exists() {
            return Ok(());
        }

        let contents = fs::read_to_string(state_path)?;

        for patch in &mut self.patches {
            let name = patch
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");

            patch.selected = contents.lines().any(|line| line == name);
        }

        Ok(())
    }

    pub fn check_patches(&mut self, srcdir: &Path) -> bool {
        let mut all_clean = true;

        for patch in &mut self.patches {
            if !patch.selected {
                continue;
            }

            if patch.check(srcdir) != PatchStatus::Clean {
                all_clean = false;
            }
        }

        all_clean
    }

    pub fn save_selection(&self) -> io::Result<()> {
        let state_path = self.path.join(".pot");

        let contents = self
            .patches
            .iter()
            .filter(|patch| patch.selected)
            .filter_map(|patch| patch.path.file_name())
            .filter_map(|name| name.to_str())
            .collect::<Vec<_>>()
            .join("\n");

        let contents = if contents.is_empty() {
            String::new()
        } else {
            format!("{contents}\n")
        };

        fs::write(state_path, contents)
    }
}

pub fn find_source_dir(srcdir: &Path, patch: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(patch).ok()?;

    let patch_paths = contents
        .lines()
        .filter_map(|line| line.strip_prefix("--- a/"))
        .filter(|path| *path != "/dev/null")
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    fn candidates(root: &Path) -> io::Result<Vec<PathBuf>> {
        let mut result = Vec::new();

        for entry in fs::read_dir(root)? {
            let path = entry?.path();

            if path.is_dir() {
                result.push(path.clone());
                result.extend(candidates(&path)?);
            }
        }

        Ok(result)
    }

    let candidates = candidates(srcdir).ok()?;
    let mut fallback = None;

    for candidate in candidates {
        for patch_path in &patch_paths {
            let components = patch_path.components().collect::<Vec<_>>();

            for index in 0..components.len() {
                let suffix = components[index..].iter().collect::<PathBuf>();

                if candidate.join(&suffix).exists() {
                    if candidate.join(".git").exists() {
                        return Some(candidate);
                    }

                    if fallback.is_none() {
                        fallback = Some(candidate.clone());
                    }
                }
            }
        }
    }

    fallback
}

impl Patch {
    pub fn check(&mut self, srcdir: &Path) -> PatchStatus {
        let source_dir = match find_source_dir(srcdir, &self.path) {
            Some(path) => path,
            None => {
                self.status = PatchStatus::Failed;
                return self.status;
            }
        };

        let status = Command::new("git")
            .args(["apply", "--check", "-p3"])
            .arg(&self.path)
            .current_dir(source_dir)
            .output();

        self.status = match status {
            Ok(output) if output.status.success() => PatchStatus::Clean,
            _ => PatchStatus::Failed,
        };

        self.status
    }
}

pub fn discover(root: &Path) -> io::Result<Vec<Recipe>> {
    let mut recipes = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let mut patches = Vec::new();

        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let patch_path = entry.path();

            if !patch_path.is_file() {
                continue;
            }

            if patch_path.extension().and_then(|ext| ext.to_str()) != Some("patch") {
                continue;
            }

            patches.push(Patch {
                path: patch_path,
                selected: false,
                status: PatchStatus::Unknown,
            });
        }

        patches.sort_by(|a, b| a.path.cmp(&b.path));

        let mut recipe = Recipe {
            name,
            path,
            patches,
        };

        recipe.load_selection()?;

        recipes.push(recipe);
    }

    recipes.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(recipes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, patch: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "pot-cookbook-test-{}-{}",
            std::process::id(),
            name
        ));

        let source_dir = root.join("src/nautilus/src");
        fs::create_dir_all(&source_dir).unwrap();

        fs::write(
            source_dir.join("nautilus-window-slot.c"),
            "original\n",
        )
        .unwrap();

        let patch_path = root.join("test.patch");
        fs::write(&patch_path, patch).unwrap();

        (root, patch_path)
    }

    #[test]
    fn returns_none_when_source_is_not_found() {
        let root = std::env::temp_dir().join(format!(
            "pot-cookbook-test-{}-missing",
            std::process::id()
        ));

        fs::create_dir_all(root.join("src/nautilus/src")).unwrap();

        let patch = root.join("missing.patch");

        assert_eq!(find_source_dir(&root.join("src"), &patch), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_source_dir() {
        let patch = "\
--- a/src/nautilus/src/nautilus-window-slot.c
+++ b/src/nautilus/src/nautilus-window-slot.c
";

        let (root, patch_path) = fixture("find", patch);
        let srcdir = root.join("src");

        assert_eq!(
            find_source_dir(&srcdir, &patch_path),
            Some(root.join("src/nautilus"))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn check_selected_patches() {
        let patch = "\
--- a/src/nautilus/src/nautilus-window-slot.c
+++ b/src/nautilus/src/nautilus-window-slot.c
@@ -1 +1 @@
-original
+changed
";

        let (root, patch_path) = fixture("selected", patch);

        let mut recipe = Recipe {
            name: "nautilus".to_string(),
            path: root.clone(),
            patches: vec![Patch {
                path: patch_path,
                selected: true,
                status: PatchStatus::Unknown,
            }],
        };

        assert!(recipe.check_patches(&root.join("src")));
        assert_eq!(recipe.patches[0].status, PatchStatus::Clean);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn check_selected_patches_ignores_unselected() {
        let root = std::env::temp_dir().join(format!(
            "pot-cookbook-test-{}-unselected",
            std::process::id()
        ));

        let mut recipe = Recipe {
            name: "test".to_string(),
            path: root.clone(),
            patches: vec![Patch {
                path: root.join("does-not-exist.patch"),
                selected: false,
                status: PatchStatus::Unknown,
            }],
        };

        assert!(recipe.check_patches(&root.join("src")));
        assert_eq!(recipe.patches[0].status, PatchStatus::Unknown);
    }

    #[test]
    fn check_patch_status() {
        let clean_patch = "\
--- a/src/nautilus/src/nautilus-window-slot.c
+++ b/src/nautilus/src/nautilus-window-slot.c
@@ -1 +1 @@
-original
+changed
";

        let (root, clean_path) = fixture("clean", clean_patch);

        let mut clean = Patch {
            path: clean_path,
            selected: true,
            status: PatchStatus::Unknown,
        };

        assert_eq!(
            clean.check(&root.join("src")),
            PatchStatus::Clean
        );

        fs::remove_dir_all(root).unwrap();

        let failed_patch = "\
--- a/src/nautilus/src/nautilus-window-slot.c
+++ b/src/nautilus/src/nautilus-window-slot.c
@@ -1 +1 @@
-does-not-exist
+changed
";

        let (root, failed_path) = fixture("failed", failed_patch);

        let mut failed = Patch {
            path: failed_path,
            selected: true,
            status: PatchStatus::Unknown,
        };

        assert_eq!(
            failed.check(&root.join("src")),
            PatchStatus::Failed
        );

        fs::remove_dir_all(root).unwrap();
    }
}


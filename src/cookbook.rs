use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    /// Not checked (yet) against the prepared sources.
    Unknown,
    /// A check is queued or running in the background.
    Checking,
    Clean,
    Failed,
}

impl Patch {
    pub fn file_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<invalid filename>")
    }
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

    fn cookbook(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("pot-cookbook-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        fs::create_dir_all(root.join("nautilus")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("nautilus/b.patch"), "").unwrap();
        fs::write(root.join("nautilus/a.patch"), "").unwrap();
        fs::write(root.join("nautilus/notes.txt"), "").unwrap();
        fs::write(root.join("stray-file"), "").unwrap();

        root
    }

    #[test]
    fn discovers_recipes_and_patches_in_order() {
        let root = cookbook("discover");

        let recipes = discover(&root).unwrap();

        assert_eq!(
            recipes
                .iter()
                .map(|recipe| recipe.name.as_str())
                .collect::<Vec<_>>(),
            ["empty", "nautilus"]
        );

        assert!(recipes[0].patches.is_empty());

        assert_eq!(
            recipes[1]
                .patches
                .iter()
                .map(Patch::file_name)
                .collect::<Vec<_>>(),
            ["a.patch", "b.patch"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selection_round_trips() {
        let root = cookbook("selection");

        let mut recipes = discover(&root).unwrap();
        let nautilus = &mut recipes[1];

        nautilus.patches[1].selected = true;
        nautilus.save_selection().unwrap();

        let reloaded = discover(&root).unwrap();

        assert!(!reloaded[1].patches[0].selected);
        assert!(reloaded[1].patches[1].selected);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_cookbook_is_an_error() {
        assert!(discover(Path::new("/definitely/not/a/cookbook")).is_err());
    }
}

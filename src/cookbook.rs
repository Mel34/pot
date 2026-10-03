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

use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug)]
pub struct Recipe {
    pub name: String,
    pub patches: Vec<std::path::PathBuf>,
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

            patches.push(patch_path);
        }

        patches.sort();

        recipes.push(Recipe { name, patches });
    }

    recipes.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(recipes)
}

mod config;
mod cookbook;
mod kitchen;
mod overlay;
mod package;
mod patch;
mod paths;
mod prepare;
mod repository;
mod ui;
mod worker;

fn main() {
    let config_path = paths::config_path();
    let first_start = !config_path.exists();

    let mut config = config::load(&config_path);
    let log = config.log.unwrap_or(false);

    let repository_name = config.repository.clone().unwrap_or_else(repository::name);

    if first_start {
        let repository_exists = repository::repository_dir(&repository_name).exists();

        let confirmed = if repository_exists {
            ui::first_start(
                "Existing Local Repository Found",
                "Use local repository:",
                &repository_name,
            )
        } else {
            ui::first_start(
                "No Local Repository Found",
                "Create local repository:",
                &repository_name,
            )
        }
        .unwrap_or(false);

        if !confirmed {
            return;
        }

        std::fs::create_dir_all(
            config_path
                .parent()
                .expect("configuration path has no parent"),
        )
        .expect("failed to create POT configuration directory");

        std::fs::write(
            &config_path,
            format!(
                r#"# POT configuration

# Path to the cookbook.
# Default: ~/Projects/cookbook
# Cookbook = ~/Projects/cookbook

# Name of the local pacman repository.
# Default: {{hostname}}
# Repository = {{hostname}}

# Interface verbosity.
# Default: false
# Log = false
"#,
            ),
        )
        .expect("failed to create POT configuration file");

        config = config::load(&config_path);
    }

    let repository_name = config.repository.clone().unwrap_or_else(repository::name);

    if first_start {
        repository::ensure_confirmed(&repository_name);
    } else {
        repository::ensure(&repository_name);
    }

    let cookbook_dir = paths::cookbook_dir(config.cookbook.as_deref());

    let recipes = match cookbook::discover(&cookbook_dir) {
        Ok(recipes) => recipes,
        Err(error) => {
            eprintln!("Failed to discover cookbook: {error}");
            std::process::exit(1);
        }
    };

    if let Err(error) = ui::run(recipes, &paths::cache_dir(), log, &repository_name) {
        eprintln!("POT interface error: {error}");
        std::process::exit(1);
    }
}

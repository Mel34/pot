mod config;
mod cookbook;
mod kitchen;
mod overlay;
mod package;
mod patch;
mod paths;
mod prepare;
mod repository;
mod startup;
mod ui;
mod worker;

fn create_config(path: &std::path::Path) -> config::Config {
    std::fs::create_dir_all(
        path.parent()
            .expect("configuration path has no parent"),
    )
    .expect("failed to create POT configuration directory");

    std::fs::write(
        path,
        r#"# POT configuration

# Path to the cookbook.
# Default: ~/Projects/cookbook
# Cookbook = ~/Projects/cookbook

# Name of the local pacman repository.
# Default: {hostname}
# Repository = {hostname}

# Interface verbosity.
# Default: false
# Log = false
"#,
    )
    .expect("failed to create POT configuration file");

    config::load(path)
}

fn main() {
    let config_path = paths::config_path();
    let mut config = config::load(&config_path);
    let log = config.log.unwrap_or(false);

    let repository_name = config.repository.clone().unwrap_or_else(repository::name);
    let repository_exists = repository::repository_dir(&repository_name).exists();

    match startup::action(config_path.exists(), repository_exists) {
        startup::Action::Normal => {
            repository::ensure(&repository_name);
        }

        startup::Action::Adopt => {
            let confirmed = ui::first_start(
                "Existing Local Repository Found",
                "Use local repository:",
                &repository_name,
            )
            .unwrap_or(false);

            if !confirmed {
                return;
            }

            config = create_config(&config_path);

            repository::ensure_confirmed(&repository_name);
        }

        startup::Action::Create => {
            let confirmed = ui::first_start(
                "No Local Repository Found",
                "Create local repository:",
                &repository_name,
            )
            .unwrap_or(false);

            if !confirmed {
                return;
            }

            config = create_config(&config_path);

            repository::ensure_confirmed(&repository_name);
        }
    }

    let repository_name = config.repository.clone().unwrap_or_else(repository::name);

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

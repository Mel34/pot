mod config;
mod cookbook;
mod kitchen;
mod overlay;
mod paths;
mod package;
mod prepare;
mod repository;
mod ui;
mod worker;

fn main() {
    let config_path = paths::config_path();

    let mut config = config::load(&config_path);
    let log = config.log.unwrap_or(false);

    if !config_path.exists() {
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

    repository::ensure(&repository_name);

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

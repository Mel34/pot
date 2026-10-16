mod config;
mod cookbook;
mod diag;
mod kitchen;
mod overlay;
mod package;
mod patch;
mod paths;
mod prepare;
mod proc;
mod repository;
mod startup;
mod helper;
mod term;
mod theme;
mod ui;
mod worker;

fn create_config(path: &std::path::Path) -> Result<config::Config, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "configuration path has no parent".to_owned())?;

    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;

    std::fs::write(
        path,
        r#"# POT configuration

# Path to the cookbook.
# Default: ~/Projects/cookbook
# Cookbook = ~/Projects/cookbook

# Name of the local pacman repository.
# Default: {hostname}
# Repository = {hostname}

# Interface verbosity: open the live log automatically when a
# prepare or cook starts.
# Default: false
# Log = false
"#,
    )
    .map_err(|error| format!("failed to write {}: {error}", path.display()))?;

    Ok(config::load(path))
}

fn run() -> Result<(), String> {
    term::install_panic_hook();

    let cache_dir = paths::cache_dir();
    diag::init(cache_dir.join("pot.log"));

    let config_path = paths::config_path();
    let mut config = config::load(&config_path);
    let log = config.log.unwrap_or(false);

    let repository_name = match config.repository.clone() {
        Some(name) => name,
        None => repository::name()?,
    };

    let repository_exists = repository::repository_dir(&repository_name)?.exists();

    match startup::action(config_path.exists(), repository_exists) {
        startup::Action::Normal => {
            repository::ensure(&repository_name)?;
        }

        startup::Action::Adopt => {
            let confirmed = ui::first_start(
                "Existing Local Repository Found",
                "Use local repository:",
                &repository_name,
            )
            .unwrap_or(false);

            if !confirmed {
                return Ok(());
            }

            config = create_config(&config_path)?;

            repository::ensure_confirmed(&repository_name)?;
        }

        startup::Action::Create => {
            let confirmed = ui::first_start(
                "No Local Repository Found",
                "Create local repository:",
                &repository_name,
            )
            .unwrap_or(false);

            if !confirmed {
                return Ok(());
            }

            config = create_config(&config_path)?;

            repository::ensure_confirmed(&repository_name)?;
        }
    }

    let repository_name = match config.repository.clone() {
        Some(name) => name,
        None => repository::name()?,
    };

    let cookbook_dir = paths::cookbook_dir(config.cookbook.as_deref());

    let recipes = cookbook::discover(&cookbook_dir)
        .map_err(|error| format!("failed to read the cookbook {}: {error}", cookbook_dir.display()))?;

    let env = worker::Env::new(cache_dir, repository_name);

    ui::run(recipes, env, log, &cookbook_dir).map_err(|error| format!("interface error: {error}"))
}

fn main() {
    if let Err(message) = run() {
        eprintln!("pot: {message}");
        std::process::exit(1);
    }
}

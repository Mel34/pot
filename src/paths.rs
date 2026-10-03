use std::env;
use std::path::PathBuf;

pub fn cache_dir() -> PathBuf {
    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home).join("pot");
    }

    PathBuf::from(env::var_os("HOME").expect("HOME is not set"))
        .join(".cache")
        .join("pot")
}

pub fn cookbook_dir() -> PathBuf {
    PathBuf::from(env::var_os("HOME").expect("HOME is not set"))
        .join("Projects")
        .join("cookbook")
}

pub fn config_path() -> PathBuf {
    PathBuf::from(env::var_os("XDG_CONFIG_HOME").unwrap_or_else(|| {
        PathBuf::from(env::var_os("HOME").expect("HOME is not set"))
            .join(".config")
            .into_os_string()
    }))
    .join("pot")
    .join("config")
}

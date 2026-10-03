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

pub fn cookbook_dir(configured: Option<&str>) -> PathBuf {
    if let Some(path) = configured {
        if path == "~" {
            return PathBuf::from(env::var_os("HOME").expect("HOME is not set"));
        }

        if let Some(path) = path.strip_prefix("~/") {
            return PathBuf::from(env::var_os("HOME").expect("HOME is not set"))
                .join(path);
        }

        let path = PathBuf::from(path);

        if path.is_absolute() {
            return path;
        }
    }

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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cookbook_path() {
        let path = cookbook_dir(None);

        assert_eq!(
            path,
            PathBuf::from(env::var_os("HOME").unwrap())
                .join("Projects")
                .join("cookbook")
        );
    }

    #[test]
    fn expands_home_path() {
        let path = cookbook_dir(Some("~/my-cookbook"));

        assert_eq!(
            path,
            PathBuf::from(env::var_os("HOME").unwrap())
                .join("my-cookbook")
        );
    }

    #[test]
    fn accepts_absolute_path() {
        let path = cookbook_dir(Some("/tmp/my-cookbook"));

        assert_eq!(path, PathBuf::from("/tmp/my-cookbook"));
    }

    #[test]
    fn rejects_relative_path() {
        let path = cookbook_dir(Some("my-cookbook"));

        assert_eq!(
            path,
            PathBuf::from(env::var_os("HOME").unwrap())
                .join("Projects")
                .join("cookbook")
        );
    }
}

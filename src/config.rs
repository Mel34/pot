use std::fs;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Config {
    pub cookbook: Option<String>,
    pub repository: Option<String>,
    pub log: Option<bool>,
}

pub fn load(path: &Path) -> Config {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => return Config::default(),
    };

    let mut config = Config::default();

    for line in contents.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();

        if value.is_empty() {
            continue;
        }

        match key {
            "Cookbook" => {
                config.cookbook = Some(value.to_owned());
            }
            "Repository" => {
                config.repository = Some(value.to_owned());
            }
            "Log" => {
                if let Ok(value) = value.parse::<bool>() {
                    config.log = Some(value);
                }
            }
            _ => {}
        }
    }

    config
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_entries() {
        let path = std::env::temp_dir().join("pot-config-test-valid");

        fs::write(
            &path,
            "\
# comment

Cookbook = /home/test/cookbook
Repository = testrepo
Log = true
",
        )
        .unwrap();

        let config = load(&path);

        assert_eq!(config.cookbook.as_deref(), Some("/home/test/cookbook"));
        assert_eq!(config.repository.as_deref(), Some("testrepo"));
        assert_eq!(config.log, Some(true));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn ignores_invalid_entries() {
        let path = std::env::temp_dir().join("pot-config-test-invalid");

        fs::write(
            &path,
            "\
not an option
Unknown = value
Cookbook =
Log = banana
Repository = valid
",
        )
        .unwrap();

        let config = load(&path);

        assert_eq!(config.cookbook, None);
        assert_eq!(config.repository.as_deref(), Some("valid"));
        assert_eq!(config.log, None);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn last_valid_value_wins() {
        let path = std::env::temp_dir().join("pot-config-test-duplicate");

        fs::write(
            &path,
            "\
Repository = first
Repository = second
Log = false
Log = true
",
        )
        .unwrap();

        let config = load(&path);

        assert_eq!(config.repository.as_deref(), Some("second"));
        assert_eq!(config.log, Some(true));

        fs::remove_file(path).unwrap();
    }
}

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub server: Option<String>,
}

pub fn repositories(path: &Path) -> io::Result<Vec<Repository>> {
    let mut repositories = Vec::new();
    let mut active_includes = HashSet::new();

    parse_file(
        path,
        &mut repositories,
        &mut active_includes,
    )?;

    Ok(repositories)
}

pub fn local_repositories(path: &Path) -> io::Result<Vec<Repository>> {
    Ok(repositories(path)?
        .into_iter()
        .filter(|repository| {
            repository
                .server
                .as_deref()
                .is_some_and(|server| server.starts_with("file://"))
        })
        .collect())
}

fn parse_file(
    path: &Path,
    repositories: &mut Vec<Repository>,
    active_includes: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let path = path.canonicalize()?;

    if !active_includes.insert(path.clone()) {
        return Ok(());
    }

    let mut section = None;
    let mut server = None;

    let result = parse_contents_inner(
        &fs::read_to_string(&path)?,
        &path,
        repositories,
        active_includes,
        &mut section,
        &mut server,
    );

    active_includes.remove(&path);

    result?;

    if let Some(name) = section {
        repositories.push(Repository { name, server });
    }

    Ok(())
}

fn parse_contents(
    contents: &str,
    path: &Path,
    repositories: &mut Vec<Repository>,
    active_includes: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let mut section = None;
    let mut server = None;

    parse_contents_inner(
        contents,
        path,
        repositories,
        active_includes,
        &mut section,
        &mut server,
    )?;

    if let Some(name) = section {
        repositories.push(Repository { name, server });
    }

    Ok(())
}

fn parse_contents_inner(
    contents: &str,
    path: &Path,
    repositories: &mut Vec<Repository>,
    active_includes: &mut HashSet<PathBuf>,
    section: &mut Option<String>,
    server: &mut Option<String>,
) -> io::Result<()> {
    for line in contents.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            if let Some(name) = section.take() {
                repositories.push(Repository {
                    name,
                    server: server.take(),
                });
            }

            let name = &line[1..line.len() - 1];

            if name == "options" {
                *section = None;
                *server = None;
            } else {
                *section = Some(name.to_owned());
                *server = None;
            }

            continue;
        }

        if let Some(include) = line.strip_prefix("Include") {
            if let Some(include) = include.trim_start().strip_prefix('=') {
                let include = include.trim();

                if !include.is_empty() {
                    let pattern = if Path::new(include).is_absolute() {
                        PathBuf::from(include)
                    } else {
                        path.parent()
                            .unwrap_or_else(|| Path::new("/"))
                            .join(include)
                    };

                    for entry in glob::glob(&pattern.to_string_lossy()).map_err(|error| {
                        io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
                    })? {
                        let entry = entry.map_err(|error| {
                            io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                        })?;

                        if entry.is_file() {
                            let entry = entry.canonicalize()?;

                            if !active_includes.insert(entry.clone()) {
                                continue;
                            }

                            let result = parse_contents_inner(
                                &fs::read_to_string(&entry)?,
                                &entry,
                                repositories,
                                active_includes,
                                section,
                                server,
                            );

                            active_includes.remove(&entry);

                            result?;
                        }
                    }
                }
            }

            continue;
        }

        if section.is_some() {
            if let Some(value) = line.strip_prefix("Server") {
                if let Some(value) = value.trim_start().strip_prefix('=') {
                    *server = Some(value.trim().to_owned());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_sections() {
        let mut repositories = Vec::new();
        let mut includes = HashSet::new();

        parse_contents(
            r#"
                [options]
                CacheDir = /var/cache/pacman

                [core]
                Server = https://example.com/core/$arch

                [local]
                SigLevel = Optional TrustAll
                Server = file:///var/cache/pacman/local
            "#,
            Path::new("/etc/pacman.conf"),
            &mut repositories,
            &mut includes,
        )
        .unwrap();

        assert_eq!(
            repositories,
            vec![
                Repository {
                    name: "core".to_owned(),
                    server: Some("https://example.com/core/$arch".to_owned()),
                },
                Repository {
                    name: "local".to_owned(),
                    server: Some("file:///var/cache/pacman/local".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn ignores_options() {
        let mut repositories = Vec::new();
        let mut includes = HashSet::new();

        parse_contents(
            r#"
                [options]
                Server = file:///not/a/repository

                [local]
                Server = file:///var/cache/pacman/local
            "#,
            Path::new("/etc/pacman.conf"),
            &mut repositories,
            &mut includes,
        )
        .unwrap();

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].name, "local");
    }

    #[test]
    fn preserves_repository_order() {
        let mut repositories = Vec::new();
        let mut includes = HashSet::new();

        parse_contents(
            r#"
                [first]
                Server = file:///first

                [second]
                Server = file:///second

                [third]
                Server = https://example.com/third/$arch
            "#,
            Path::new("/etc/pacman.conf"),
            &mut repositories,
            &mut includes,
        )
        .unwrap();

        let names: Vec<_> = repositories
            .iter()
            .map(|repository| repository.name.as_str())
            .collect();

        assert_eq!(names, ["first", "second", "third"]);
    }

    #[test]
    fn finds_local_repositories() {
        let mut repositories = Vec::new();
        let mut includes = HashSet::new();

        parse_contents(
            r#"
                [remote]
                Server = https://example.com/core/$arch

                [local]
                Server = file:///var/cache/pacman/local

                [another-local]
                Server = file:///srv/pacman/another
            "#,
            Path::new("/etc/pacman.conf"),
            &mut repositories,
            &mut includes,
        )
        .unwrap();

        let local: Vec<_> = repositories
            .into_iter()
            .filter(|repository| {
                repository
                    .server
                    .as_deref()
                    .is_some_and(|server| server.starts_with("file://"))
            })
            .collect();

        assert_eq!(
            local,
            vec![
                Repository {
                    name: "local".to_owned(),
                    server: Some("file:///var/cache/pacman/local".to_owned()),
                },
                Repository {
                    name: "another-local".to_owned(),
                    server: Some("file:///srv/pacman/another".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn follows_nested_includes_and_preserves_order() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let dir = std::env::temp_dir().join(format!(
            "pot-pacman-test-{}-{timestamp}",
            std::process::id()
        ));

        fs::create_dir_all(&dir).unwrap();

        let main = dir.join("pacman.conf");
        let pot = dir.join("pot.conf");
        let repositories_config = dir.join("repositories.conf");
        let mirrorlist = dir.join("mirrorlist");

        fs::write(
            &main,
            r#"
                [options]
                Include = pot.conf

                [core]
                Include = mirrorlist

                [extra]
                Server = https://example.com/extra/$arch
            "#,
        )
        .unwrap();

        fs::write(
            &pot,
            r#"
                Include = repositories.conf
            "#,
        )
        .unwrap();

        fs::write(
            &repositories_config,
            r#"
                [pot-test]
                Server = file:///var/cache/pacman/pot-test

                [alimyon]
                Server = file:///var/cache/pacman/alimyon
            "#,
        )
        .unwrap();

        fs::write(
            &mirrorlist,
            r#"
                Server = https://mirror.example.com/core/$arch
            "#,
        )
        .unwrap();

        let repositories = repositories(&main).unwrap();

        assert_eq!(
            repositories,
            vec![
                Repository {
                    name: "pot-test".to_owned(),
                    server: Some("file:///var/cache/pacman/pot-test".to_owned()),
                },
                Repository {
                    name: "alimyon".to_owned(),
                    server: Some("file:///var/cache/pacman/alimyon".to_owned()),
                },
                Repository {
                    name: "core".to_owned(),
                    server: Some("https://mirror.example.com/core/$arch".to_owned()),
                },
                Repository {
                    name: "extra".to_owned(),
                    server: Some("https://example.com/extra/$arch".to_owned()),
                },
            ]
        );

        let local = local_repositories(&main).unwrap();

        assert_eq!(
            local,
            vec![
                Repository {
                    name: "pot-test".to_owned(),
                    server: Some("file:///var/cache/pacman/pot-test".to_owned()),
                },
                Repository {
                    name: "alimyon".to_owned(),
                    server: Some("file:///var/cache/pacman/alimyon".to_owned()),
                },
            ]
        );

        fs::remove_dir_all(dir).unwrap();
    }
}

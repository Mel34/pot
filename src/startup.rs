#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Normal,
    Adopt,
    Create,
}

pub fn action(config_exists: bool, repository_exists: bool) -> Action {
    if config_exists {
        Action::Normal
    } else if repository_exists {
        Action::Adopt
    } else {
        Action::Create
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_config_starts_normally() {
        assert_eq!(action(true, true), Action::Normal);
        assert_eq!(action(true, false), Action::Normal);
    }

    #[test]
    fn missing_config_with_existing_repository_adopts() {
        assert_eq!(action(false, true), Action::Adopt);
    }

    #[test]
    fn missing_config_without_repository_creates() {
        assert_eq!(action(false, false), Action::Create);
    }
}

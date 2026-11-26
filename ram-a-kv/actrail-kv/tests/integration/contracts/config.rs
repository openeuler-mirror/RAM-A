//! Exercises the TOML contract through a concrete root configuration.

use std::fs;
use std::path::{Path, PathBuf};

use config_contract::{toml_edit, Config, ConfigError, ConfigState, RootConfig, Table};
use tempfile::tempdir;
use toml_edit::{value, DocumentMut};

struct ReceiverConfig {
    port: i64,
}

impl Config for ReceiverConfig {
    fn parse_toml_table(table: &Table) -> Result<(Self, ConfigState), ConfigError> {
        let (port, state) = match table.get("port") {
            Some(item) => (
                item.as_integer()
                    .ok_or_else(|| ConfigError::InvalidValue("port must be an integer".into()))?,
                ConfigState::Clean,
            ),
            None => (8080, ConfigState::Dirty),
        };
        Ok((Self { port }, state))
    }

    fn to_toml_table(&self) -> Table {
        let mut table = Table::new();
        table.insert("port", value(self.port));
        table
    }
}

impl RootConfig for ReceiverConfig {}

#[test]
fn fills_defaults_and_backs_up_exact_contents_before_updating() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let original =
        b"[actrail.receiver]\r\n# default port\r\n\r\n[other] # keep\r\nname  = 'example'\r\n";
    fs::write(&path, original).unwrap();

    let locator = ["actrail", "receiver"];
    let config = ReceiverConfig::load(&path, &locator).unwrap();
    assert_eq!(config.port, 8080);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(backups(directory.path()).is_empty());

    let mut config = ReceiverConfig::load_and_save(&path, &locator).unwrap();
    assert_eq!(config.port, 8080);
    let first_backup = &backups(directory.path())[0];
    assert_eq!(fs::read(first_backup).unwrap(), original);
    let first_save = fs::read_to_string(&path).unwrap();
    assert!(first_save.contains("[other] # keep\nname  = 'example'"));
    let persisted = first_save.parse::<DocumentMut>().unwrap();
    assert_eq!(
        persisted["actrail"]["receiver"]["port"].as_integer(),
        Some(8080)
    );

    ReceiverConfig::load_and_save(&path, &locator).unwrap();
    assert_eq!(backups(directory.path()).len(), 1);
    assert_eq!(fs::read_to_string(&path).unwrap(), first_save);

    config.port = 9090;
    config.update(&path, &locator).unwrap();
    let all_backups = backups(directory.path());
    assert_eq!(all_backups.len(), 2);
    let second_backup = all_backups
        .iter()
        .find(|path| *path != first_backup)
        .unwrap();
    assert_eq!(fs::read_to_string(second_backup).unwrap(), first_save);
    assert_eq!(ReceiverConfig::load(&path, &locator).unwrap().port, 9090);
}

#[test]
fn creates_missing_files_and_nested_tables_on_save() {
    let directory = tempdir().unwrap();
    let parent = directory.path().join("not-created-yet");
    let path = parent.join("settings.toml");
    let locator = ["components", "actrail", "receiver"];

    assert_eq!(ReceiverConfig::load(&path, &locator).unwrap().port, 8080);
    assert!(!parent.exists());

    ReceiverConfig::load_and_save(&path, &locator).unwrap();
    assert!(backups(&parent).is_empty());
    let reloaded = ReceiverConfig::load(&path, &locator).unwrap();
    assert_eq!(reloaded.port, 8080);
}

#[test]
fn supports_a_root_locator_and_rejects_invalid_table_paths() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let original = "port = 1234\n";
    fs::write(&path, original).unwrap();

    assert!(matches!(
        ReceiverConfig::load(&path, &["port", "receiver"]),
        Err(ConfigError::ExpectedTable(key)) if key == "port"
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    let mut root = ReceiverConfig::load(&path, &[]).unwrap();
    assert_eq!(root.port, 1234);
    root.port = 4321;
    let destination = directory.path().join("other.toml");
    fs::write(&destination, "port = 5555\n[other]\nvalue = true\n").unwrap();

    assert!(matches!(
        root.update(&destination, &["port"]),
        Err(ConfigError::ExpectedTable(key)) if key == "port"
    ));
    root.update(&destination, &["receiver"]).unwrap();
    let saved = fs::read_to_string(&destination).unwrap();
    let document = saved.parse::<DocumentMut>().unwrap();
    assert_eq!(document["receiver"]["port"].as_integer(), Some(4321));
    assert_eq!(document["other"]["value"].as_bool(), Some(true));
    assert_eq!(document["port"].as_integer(), Some(5555));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    root.update(&destination, &[]).unwrap();
    assert_eq!(ReceiverConfig::load(&destination, &[]).unwrap().port, 4321);
    assert_eq!(fs::read_to_string(&destination).unwrap(), "port = 4321\n");
}

fn backups(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bak"))
        .collect()
}

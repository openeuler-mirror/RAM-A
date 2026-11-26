//! Defines nested TOML configuration and persists the selected root table.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use tempfile::NamedTempFile;
use thiserror::Error;
use time::OffsetDateTime;
use toml_edit::{DocumentMut, Item, Table};

/// Whether parsing supplied defaults that should be written back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigState {
    Clean,
    Dirty,
}

/// A failure while parsing or persisting a configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration file I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid TOML document: {0}")]
    Toml(#[from] toml_edit::TomlError),
    #[error("expected a TOML table at {0}")]
    ExpectedTable(String),
    #[error("invalid configuration: {0}")]
    InvalidValue(String),
}

/// A configuration unit that can be nested in another TOML table.
pub trait Config: Sized {
    /// Parses a table and reports whether defaults need to be written back.
    fn parse_toml_table(table: &Table) -> Result<(Self, ConfigState), ConfigError>;

    /// Builds a self-contained table, including any comments supplied by this unit.
    ///
    /// Derived implementations panic if the value cannot be represented in TOML.
    /// Use `try_to_toml_table` to handle that error.
    fn to_toml_table(&self) -> Table;

    /// Builds a self-contained table and reports any serialization error.
    fn try_to_toml_table(&self) -> Result<Table, ConfigError> {
        Ok(self.to_toml_table())
    }

    #[doc(hidden)]
    fn prepare_defaults(
        _input: &dyn toml_edit::TableLike,
        _defaults: &mut dyn toml_edit::TableLike,
    ) {
    }

    #[doc(hidden)]
    fn recognizes_field(_key: &str) -> bool {
        true
    }

    #[doc(hidden)]
    fn collect_unknown_in_field(_key: &str, _item: &Item, _path: &str, _unknown: &mut Vec<String>) {
    }

    #[doc(hidden)]
    fn collect_unknown_fields(
        table: &dyn toml_edit::TableLike,
        prefix: &str,
        unknown: &mut Vec<String>,
    ) {
        for (key, item) in table.iter() {
            let path = if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            };
            if Self::recognizes_field(key) {
                Self::collect_unknown_in_field(key, item, &path, unknown);
            } else {
                unknown.push(path);
            }
        }
    }

    #[doc(hidden)]
    fn add_toml_comments(&self, _table: &mut Table) {}
}

/// A configuration unit that can be loaded from and saved to a TOML file.
pub trait RootConfig: Config {
    /// Reads the selected table without writing to disk.
    /// An empty locator selects the document root.
    fn load(path: &Path, locator: &[&str]) -> Result<Self, ConfigError> {
        let document = read_document(path)?.unwrap_or_default();
        let (config, _) = parse_config_at::<Self>(document.as_table(), locator)?;
        Ok(config)
    }

    /// Reads once and writes back if the file or table is missing or defaults were supplied.
    /// An empty locator selects the document root.
    fn load_and_save(path: &Path, locator: &[&str]) -> Result<Self, ConfigError> {
        let document = read_document(path)?;
        let exists = document.is_some();
        let mut document = document.unwrap_or_default();
        let (config, state) = parse_config_at::<Self>(document.as_table(), locator)?;
        if !exists || state == ConfigState::Dirty {
            let table = navigate_mut(document.as_table_mut(), locator)?;
            let mut defaults = config.try_to_toml_table()?;
            Self::prepare_defaults(table, &mut defaults);
            crate::derive_support::fill_missing_fields(table, &defaults);
            write_document(path, &document, exists)?;
        }
        Ok(config)
    }

    /// Reads the destination and replaces the selected table with this configuration.
    /// An empty locator selects the document root.
    fn update(&self, path: &Path, locator: &[&str]) -> Result<(), ConfigError> {
        let document = read_document(path)?;
        let exists = document.is_some();
        let mut document = document.unwrap_or_default();
        *navigate_mut(document.as_table_mut(), locator)? = self.try_to_toml_table()?;
        write_document(path, &document, exists)
    }
}

fn read_document(path: &Path) -> Result<Option<DocumentMut>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content.parse()?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn parse_config_at<C: Config>(
    table: &Table,
    locator: &[&str],
) -> Result<(C, ConfigState), ConfigError> {
    match navigate(table, locator)? {
        Some(table) => {
            let parsed = C::parse_toml_table(table)?;
            let mut unknown = Vec::new();
            C::collect_unknown_fields(table, &locator.join("."), &mut unknown);
            for path in unknown {
                eprintln!("warning: unknown configuration field `{path}` (ignored)");
            }
            Ok(parsed)
        }
        None => {
            let (config, _) = C::parse_toml_table(&Table::new())?;
            Ok((config, ConfigState::Dirty))
        }
    }
}

fn write_document(path: &Path, document: &DocumentMut, exists: bool) -> Result<(), ConfigError> {
    let path = if exists {
        fs::canonicalize(path)?
    } else {
        std::path::absolute(path)?
    };
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration path needs a parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut candidate = NamedTempFile::new_in(parent)?;
    candidate.write_all(document.to_string().as_bytes())?;
    candidate.as_file().sync_all()?;
    if exists {
        backup(&path, parent)?;
    }
    candidate.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn navigate<'a>(mut table: &'a Table, locator: &[&str]) -> Result<Option<&'a Table>, ConfigError> {
    for (index, key) in locator.iter().enumerate() {
        let Some(item) = table.get(key) else {
            return Ok(None);
        };
        table = item
            .as_table()
            .ok_or_else(|| ConfigError::ExpectedTable(locator[..=index].join(".")))?;
    }
    Ok(Some(table))
}

fn navigate_mut<'a>(
    mut table: &'a mut Table,
    locator: &[&str],
) -> Result<&'a mut Table, ConfigError> {
    for (index, key) in locator.iter().enumerate() {
        table = table
            .entry(key)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigError::ExpectedTable(locator[..=index].join(".")))?;
    }
    Ok(table)
}

fn backup(path: &Path, parent: &Path) -> io::Result<()> {
    let stem = path.file_stem().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration path needs a file name",
        )
    })?;
    let now = OffsetDateTime::now_utc();
    let timestamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    );

    let mut candidate = NamedTempFile::new_in(parent)?;
    fs::copy(path, candidate.path())?;
    candidate.as_file().sync_all()?;
    for index in 1..=u64::MAX {
        let mut name = stem.to_os_string();
        name.push(format!(".{timestamp}.{index:03}"));
        if let Some(extension) = path.extension() {
            name.push(".");
            name.push(extension);
        }
        name.push(".bak");
        match candidate.persist_noclobber(parent.join(name)) {
            Ok(_) => return Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                candidate = error.file;
            }
            Err(error) => return Err(error.error),
        }
    }
    Err(io::Error::other("configuration backup index exhausted"))
}

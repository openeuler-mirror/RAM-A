//! Bridges the derive macro to Serde and the editable TOML document.

mod comments;
mod completion;
mod fields;
mod state;

use serde::{de::DeserializeOwned, Serialize};
use toml_edit::visit_mut::{self, VisitMut};
use toml_edit::{DocumentMut, Item, Table};

pub use comments::{Field, FieldComments};
pub use completion::fill_missing_fields;
pub use fields::{FieldKeys, FieldSchema};

use crate::{Config, ConfigError, ConfigState};

pub fn parse<C: Config + DeserializeOwned + Serialize>(
    table: &Table,
) -> Result<(C, ConfigState), ConfigError> {
    let document = DocumentMut::from(table.clone());
    let config: C = toml_edit::de::from_document(document).map_err(invalid_value)?;
    let mut output = toml_edit::ser::to_document(&config).map_err(invalid_value)?;
    C::prepare_defaults(table, output.as_table_mut());
    let state = if state::has_missing_keys(table, output.as_table()) {
        ConfigState::Dirty
    } else {
        ConfigState::Clean
    };
    Ok((config, state))
}

pub fn serialize<C: Config + Serialize>(config: &C) -> Result<Table, ConfigError> {
    let mut document = toml_edit::ser::to_document(config).map_err(invalid_value)?;
    ExpandTables.visit_document_mut(&mut document);
    let mut table = document.into_table();
    config.add_toml_comments(&mut table);
    Ok(table)
}

fn invalid_value(error: impl std::fmt::Display) -> ConfigError {
    ConfigError::InvalidValue(error.to_string())
}

struct ExpandTables;

impl VisitMut for ExpandTables {
    fn visit_item_mut(&mut self, item: &mut Item) {
        if item.is_value() {
            *item = std::mem::take(item)
                .into_table()
                .map(Item::Table)
                .or_else(|value| value.into_array_of_tables().map(Item::ArrayOfTables))
                .unwrap_or_else(|value| value);
        }
        visit_mut::visit_item_mut(self, item);
    }
}

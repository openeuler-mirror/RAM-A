//! Uses derived field metadata to inspect nested configurations without treating map keys as fields.
use std::marker::PhantomData;

use toml_edit::{Item, TableLike};

use crate::Config;

pub struct FieldSchema<T>(PhantomData<T>);
impl<T> FieldSchema<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

pub trait FieldKeys: Sized {
    fn prepare_table(self, _input: &dyn TableLike, _defaults: &mut dyn TableLike) {}
    fn prepare_nested(self, _input: &Item, _defaults: &mut Item) {}

    fn recognizes_field(self, key: &str) -> bool;
    fn collect_field(self, key: &str, item: &Item, path: &str, unknown: &mut Vec<String>);
    fn collect_nested(self, item: &Item, path: &str, unknown: &mut Vec<String>);
}

// Ordinary Serde values may contain dynamic map keys, so only Config types
// supply a closed field set for diagnostics.
impl<T> FieldKeys for &FieldSchema<T> {
    fn recognizes_field(self, _key: &str) -> bool {
        true
    }
    fn collect_field(self, _key: &str, _item: &Item, _path: &str, _unknown: &mut Vec<String>) {}
    fn collect_nested(self, _item: &Item, _path: &str, _unknown: &mut Vec<String>) {}
}

impl<T: Config> FieldKeys for FieldSchema<T> {
    fn prepare_table(self, input: &dyn TableLike, defaults: &mut dyn TableLike) {
        T::prepare_defaults(input, defaults);
    }
    fn prepare_nested(self, input: &Item, defaults: &mut Item) {
        if let (Some(input), Some(defaults)) = (input.as_table_like(), defaults.as_table_like_mut())
        {
            T::prepare_defaults(input, defaults);
        }
    }

    fn recognizes_field(self, key: &str) -> bool {
        T::recognizes_field(key)
    }
    fn collect_field(self, key: &str, item: &Item, path: &str, unknown: &mut Vec<String>) {
        T::collect_unknown_in_field(key, item, path, unknown);
    }
    fn collect_nested(self, item: &Item, path: &str, unknown: &mut Vec<String>) {
        if let Some(table) = item.as_table_like() {
            T::collect_unknown_fields(table, path, unknown);
        }
    }
}

impl<T: Config> FieldKeys for FieldSchema<Option<T>> {
    fn prepare_table(self, input: &dyn TableLike, defaults: &mut dyn TableLike) {
        T::prepare_defaults(input, defaults);
    }
    fn prepare_nested(self, input: &Item, defaults: &mut Item) {
        if let (Some(input), Some(defaults)) = (input.as_table_like(), defaults.as_table_like_mut())
        {
            T::prepare_defaults(input, defaults);
        }
    }

    fn recognizes_field(self, key: &str) -> bool {
        T::recognizes_field(key)
    }
    fn collect_field(self, key: &str, item: &Item, path: &str, unknown: &mut Vec<String>) {
        T::collect_unknown_in_field(key, item, path, unknown);
    }
    fn collect_nested(self, item: &Item, path: &str, unknown: &mut Vec<String>) {
        FieldSchema::<T>::new().collect_nested(item, path, unknown);
    }
}

impl<T: Config> FieldKeys for FieldSchema<Vec<T>> {
    fn prepare_nested(self, input: &Item, defaults: &mut Item) {
        let input_table = |index| -> Option<&dyn TableLike> {
            if let Some(tables) = input.as_array_of_tables() {
                tables.get(index).map(|table| table as &dyn TableLike)
            } else {
                input
                    .as_array()?
                    .get(index)?
                    .as_inline_table()
                    .map(|table| table as &dyn TableLike)
            }
        };
        if let Some(tables) = defaults.as_array_of_tables_mut() {
            for (index, table) in tables.iter_mut().enumerate() {
                if let Some(input) = input_table(index) {
                    T::prepare_defaults(input, table);
                }
            }
        } else if let Some(values) = defaults.as_array_mut() {
            for (index, value) in values.iter_mut().enumerate() {
                if let (Some(input), Some(table)) =
                    (input_table(index), value.as_inline_table_mut())
                {
                    T::prepare_defaults(input, table);
                }
            }
        }
    }

    fn recognizes_field(self, _key: &str) -> bool {
        true
    }
    fn collect_field(self, _key: &str, _item: &Item, _path: &str, _unknown: &mut Vec<String>) {}
    fn collect_nested(self, item: &Item, path: &str, unknown: &mut Vec<String>) {
        if let Some(tables) = item.as_array_of_tables() {
            for (index, table) in tables.iter().enumerate() {
                T::collect_unknown_fields(table, &format!("{path}[{index}]"), unknown);
            }
        } else if let Some(values) = item.as_array() {
            for (index, value) in values.iter().enumerate() {
                if let Some(table) = value.as_inline_table() {
                    T::collect_unknown_fields(table, &format!("{path}[{index}]"), unknown);
                }
            }
        }
    }
}

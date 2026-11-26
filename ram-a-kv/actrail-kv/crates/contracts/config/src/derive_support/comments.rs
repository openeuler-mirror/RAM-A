//! Places field comments on TOML entries and visits nested configuration values.

use toml_edit::{Item, Table};

use crate::Config;

pub struct Field<'a, T: ?Sized>(&'a T);

impl<'a, T: ?Sized> Field<'a, T> {
    pub fn new(value: &'a T) -> Self {
        Self(value)
    }
}

pub trait FieldComments {
    fn add_comments(self, table: &mut Table, name: Option<&str>, docs: &str);
}

// Method lookup uses the reference implementation for ordinary Serde values.
impl<T: ?Sized> FieldComments for &Field<'_, T> {
    fn add_comments(self, table: &mut Table, name: Option<&str>, docs: &str) {
        if let Some(name) = name {
            add_field_comment(table, name, docs);
        }
    }
}

impl<T: Config> FieldComments for Field<'_, T> {
    fn add_comments(self, table: &mut Table, name: Option<&str>, docs: &str) {
        let Some(name) = name else {
            self.0.add_toml_comments(table);
            return;
        };
        add_field_comment(table, name, docs);
        if let Some(child) = table.get_mut(name).and_then(Item::as_table_mut) {
            self.0.add_toml_comments(child);
        }
    }
}

impl<T: Config> FieldComments for Field<'_, Option<T>> {
    fn add_comments(self, table: &mut Table, name: Option<&str>, docs: &str) {
        if let Some(value) = self.0 {
            Field::new(value).add_comments(table, name, docs);
        }
    }
}

impl<T: Config> FieldComments for Field<'_, Vec<T>> {
    fn add_comments(self, table: &mut Table, name: Option<&str>, docs: &str) {
        let Some(name) = name else {
            return;
        };
        add_field_comment(table, name, docs);
        if let Some(children) = table.get_mut(name).and_then(Item::as_array_of_tables_mut) {
            for (value, child) in self.0.iter().zip(children.iter_mut()) {
                value.add_toml_comments(child);
            }
        }
    }
}

fn add_field_comment(table: &mut Table, name: &str, docs: &str) {
    if docs.is_empty() {
        return;
    }
    match table.get_mut(name) {
        Some(Item::Table(child)) => child.decor_mut().set_prefix(format!("\n{docs}")),
        Some(Item::ArrayOfTables(children)) => {
            if let Some(first) = children.get_mut(0) {
                first.decor_mut().set_prefix(format!("\n{docs}"));
            }
        }
        Some(Item::Value(_)) => {
            if let Some(mut key) = table.key_mut(name) {
                key.leaf_decor_mut().set_prefix(docs);
            }
        }
        Some(Item::None) | None => {}
    }
}

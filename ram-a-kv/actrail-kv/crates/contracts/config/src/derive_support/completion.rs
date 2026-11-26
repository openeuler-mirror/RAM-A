//! Fills missing configuration values without replacing existing values or comments.
use toml_edit::{Item, TableLike};

pub fn fill_missing_fields(table: &mut dyn TableLike, defaults: &dyn TableLike) -> bool {
    let mut changed = false;
    for (key, value) in defaults.iter() {
        if let Some(existing) = table.get_mut(key) {
            changed |= fill_item(existing, value);
        } else if let Some((key, value)) = defaults.get_key_value(key) {
            table.entry_format(key).or_insert(value.clone());
            changed = true;
        }
    }
    changed
}

fn fill_item(existing: &mut Item, defaults: &Item) -> bool {
    if let (Some(existing), Some(defaults)) =
        (existing.as_table_like_mut(), defaults.as_table_like())
    {
        return fill_missing_fields(existing, defaults);
    }
    let default_table = |index| -> Option<&dyn TableLike> {
        if let Some(tables) = defaults.as_array_of_tables() {
            tables.get(index).map(|table| table as &dyn TableLike)
        } else {
            defaults
                .as_array()?
                .get(index)?
                .as_inline_table()
                .map(|table| table as &dyn TableLike)
        }
    };
    let mut changed = false;
    if let Some(tables) = existing.as_array_of_tables_mut() {
        for (index, table) in tables.iter_mut().enumerate() {
            if let Some(defaults) = default_table(index) {
                changed |= fill_missing_fields(table, defaults);
            }
        }
    } else if let Some(values) = existing.as_array_mut() {
        for (index, value) in values.iter_mut().enumerate() {
            if let (Some(table), Some(defaults)) =
                (value.as_inline_table_mut(), default_table(index))
            {
                changed |= fill_missing_fields(table, defaults);
            }
        }
    }
    changed
}

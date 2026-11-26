//! Detects values supplied during deserialization that need to be written back.

use toml_edit::{Item, TableLike, Value};

pub(super) fn has_missing_keys(input: &dyn TableLike, output: &dyn TableLike) -> bool {
    output.iter().any(|(key, value)| {
        input
            .get(key)
            .is_none_or(|input| missing_in_item(input, value))
    })
}

fn missing_in_item(input: &Item, output: &Item) -> bool {
    if let (Some(input), Some(output)) = (input.as_table_like(), output.as_table_like()) {
        return has_missing_keys(input, output);
    }
    let Some(output) = output.as_array() else {
        return false;
    };
    if let Some(input) = input.as_array() {
        return input.iter().zip(output.iter()).any(|(input, output)| {
            match (input.as_inline_table(), output.as_inline_table()) {
                (Some(input), Some(output)) => has_missing_keys(input, output),
                _ => false,
            }
        });
    }
    input.as_array_of_tables().is_some_and(|input| {
        input
            .iter()
            .zip(output.iter().map(Value::as_inline_table))
            .any(|(input, output)| output.is_some_and(|output| has_missing_keys(input, output)))
    })
}

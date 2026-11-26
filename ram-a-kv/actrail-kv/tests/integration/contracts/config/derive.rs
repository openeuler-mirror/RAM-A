//! Exercises derived business configurations through the public TOML contract.

use std::fs;
use std::path::Path;

use config_contract::{toml_edit::DocumentMut, Config, ConfigError, ConfigState, RootConfig};
use serde::Serialize;
use tempfile::tempdir;

#[derive(Debug, PartialEq, Serialize, Config)]
struct A {
    /// Nested configuration
    #[serde(rename = "B", alias = "legacy_b")]
    b: B,
    /// Root setting declared after the nested configuration
    #[serde(alias = "legacy_message")]
    message: String,
    /// Optional setting
    #[serde(skip_serializing_if = "Option::is_none")]
    optional: Option<bool>,
}

impl RootConfig for A {}

#[derive(Debug, PartialEq, Serialize, Config)]
#[serde(rename_all = "UPPERCASE")]
struct B {
    /// C comes from the inner business type
    #[serde(default = "default_c")]
    c: u16,
    /// Another level of configuration
    deeper: Option<Inner>,
}

#[derive(Debug, PartialEq, Serialize, Config)]
struct Inner {
    /// Deepest field
    enabled: bool,
}

fn default_c() -> u16 {
    456
}

#[test]
fn reads_nested_tables_and_writes_comments_at_the_serialized_keys() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.toml");
    fs::write(
        &source,
        "A.message = \"the first line\\nthe second line\"\nA.B.C = 123\nA.B.DEEPER.enabled = true\n",
    )
    .unwrap();
    let config = A::load(&source, &["A"]).unwrap();
    assert_eq!(
        config,
        A {
            b: B {
                c: 123,
                deeper: Some(Inner { enabled: true }),
            },
            message: "the first line\nthe second line".into(),
            optional: None,
        }
    );

    let output = directory.path().join("output.toml");
    config.update(&output, &["A"]).unwrap();
    let rendered = fs::read_to_string(&output).unwrap();
    assert!(rendered.contains("# Nested configuration\n[A.B]\n"));
    assert!(rendered.contains("# C comes from the inner business type\nC = 123\n"));
    assert!(rendered.contains("# Deepest field\nenabled = true\n"));
    assert!(rendered.contains("# Root setting declared after the nested configuration\nmessage ="));
    assert!(!rendered.contains("Optional setting"));
    assert_eq!(A::load(&output, &["A"]).unwrap(), config);

    let standalone = DocumentMut::from(config.b.try_to_toml_table().unwrap()).to_string();
    assert!(standalone.contains("# Deepest field\nenabled = true\n"));
}

#[test]
fn writes_nested_defaults_once_and_reports_invalid_input_without_writing() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    let original = b"[A]\nmessage = \"host\"\nfuture = true # keep future\n[A.B]\nfuture_inner = 7\n\n[other] # keep\nvalue = true\n";
    fs::write(&path, original).unwrap();

    assert_eq!(A::load(&path, &["A"]).unwrap().b.c, 456);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(A::load_and_save(&path, &["A"]).unwrap().b.c, 456);
    let written = fs::read_to_string(&path).unwrap();
    assert!(written.contains("C = 456"));
    assert!(written.contains("future = true # keep future"));
    assert!(written.contains("future_inner = 7"));
    assert!(written.contains("[other] # keep\nvalue = true\n"));
    A::load_and_save(&path, &["A"]).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), written);
    assert_eq!(backups(directory.path()), vec![original.to_vec()]);

    let invalid = b"[A]\nmessage = \"host\"\n[A.B]\nC = \"invalid\"\n";
    fs::write(&path, invalid).unwrap();
    assert!(matches!(
        A::load_and_save(&path, &["A"]),
        Err(ConfigError::InvalidValue(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), invalid);
    assert_eq!(backups(directory.path()), vec![original.to_vec()]);
}

#[test]
fn rejects_unrepresentable_values_without_replacing_the_file() {
    #[derive(Serialize, Config)]
    struct Counter {
        count: u64,
    }

    impl RootConfig for Counter {}

    let directory = tempdir().unwrap();
    let path = directory.path().join("counter.toml");
    let original = b"count = 42\n";
    fs::write(&path, original).unwrap();

    let config = Counter { count: u64::MAX };
    assert!(matches!(
        config.update(&path, &[]),
        Err(ConfigError::InvalidValue(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(backups(directory.path()).is_empty());
}

#[test]
fn reads_business_defaults_inside_arrays_of_configuration_tables() {
    #[derive(Serialize, Config)]
    #[serde(default)]
    struct Limit<T> {
        /// Retry count
        count: T,
    }

    impl Default for Limit<u16> {
        fn default() -> Self {
            Self { count: 7 }
        }
    }

    #[derive(Serialize, Config)]
    struct Batch {
        limits: Vec<Limit<u16>>,
    }

    let input: DocumentMut = "[[limits]]\ncount = 3\n[[limits]]\n".parse().unwrap();
    let (config, state) = Batch::parse_toml_table(input.as_table()).unwrap();
    assert_eq!(state, ConfigState::Dirty);
    assert_eq!(config.limits[0].count, 3);
    assert_eq!(config.limits[1].count, 7);

    let output = DocumentMut::from(config.try_to_toml_table().unwrap());
    assert_eq!(output.to_string().matches("# Retry count").count(), 2);
    let (config, state) = Batch::parse_toml_table(output.as_table()).unwrap();
    assert_eq!(state, ConfigState::Clean);
    assert_eq!(config.limits[1].count, 7);
}

fn backups(directory: &Path) -> Vec<Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bak"))
        .map(|path| fs::read(path).unwrap())
        .collect()
}

#[test]
fn completes_inline_configuration_without_losing_aliases_or_future_fields() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("settings.toml");
    fs::write(
        &path,
        "[A]\nlegacy_message = \"host\"\nlegacy_b = { future_inner = 7 }\nfuture = true # keep\n",
    )
    .unwrap();
    let first = A::load_and_save(&path, &["A"]).unwrap();
    let written = fs::read_to_string(&path).unwrap();
    assert!(written.contains("legacy_message"));
    assert!(written.contains("legacy_b"));
    assert!(written.contains("future_inner = 7"));
    assert!(written.contains("future = true # keep"));
    assert_eq!(A::load_and_save(&path, &["A"]).unwrap(), first);
    assert_eq!(fs::read_to_string(&path).unwrap(), written);
}

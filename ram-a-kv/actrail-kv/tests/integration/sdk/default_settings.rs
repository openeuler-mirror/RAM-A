//! Verifies default-file loading and typed overrides through real libraries in an isolated process.
use actrail_kv_sdk::*;
use serde_json::Value;
use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    process::Command,
};

fn config(component: &str, output: &Path, overrides: SdkOverrides) -> SdkConfig {
    let libraries = std::env::var_os("ACTRAIL_SDK_LIB_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned()
        });
    SdkConfig {
        library_path: libraries.join(format!("libactrail_{component}.so")),
        base_dir: output.to_owned(),
        config_toml: None,
        overrides,
    }
}

fn overrides() -> SdkOverrides {
    SdkOverrides {
        storage: Some(StorageConfigOverrides {
            retention: Some(StorageRetentionConfigOverrides {
                max_size_bytes: NonZeroU64::new(1024 * 1024),
                ..Default::default()
            }),
            backends: Some(StorageBackendConfigsOverrides {
                sqlite: Some(SqliteConfigOverrides {
                    path: Some("override.sqlite".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        analyzer: Some(AnalyzerSettingsOverrides {
            selection: Some(SelectionSettingsOverrides {
                session_keys: Some(vec!["example".into()]),
                last_n: NonZeroUsize::new(4),
                ..Default::default()
            }),
            ..Default::default()
        }),
        report: Some(ReportSettingsOverrides {
            locale: Some("zh-CN".into()),
            ..Default::default()
        }),
    }
}

#[test]
fn default_file_and_typed_overrides() {
    if std::env::var_os("ACTRAIL_KV_SETTINGS_CHILD").is_none() {
        let home = tempfile::tempdir().unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "default_file_and_typed_overrides", "--nocapture"])
            .env("HOME", home.path())
            .env("ACTRAIL_KV_SETTINGS_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let output = tempfile::tempdir().unwrap();
    let settings_dir = std::env::home_dir().unwrap().join(".actrail/actrail-kv");
    let settings = settings_dir.join("settings.toml");
    // A missing file is created through the same completion path as the CLI.
    ReceiverFactory::create(config("receiver", output.path(), Default::default()))
        .unwrap()
        .close()
        .unwrap();
    assert!(settings.exists());
    assert!(settings_dir.join("captures.sqlite").exists());
    std::fs::write(
        &settings,
        r#"
[storage.retention]
max_size_bytes = 2097152
[storage.backends.sqlite]
path = "base.sqlite"
[analyzer.selection]
session_keys = ["excluded"]
last_n = 1
[report]
format = "md"
locale = "en-US"
"#,
    )
    .unwrap();

    let mut receiver =
        ReceiverFactory::create(config("receiver", output.path(), overrides())).unwrap();
    for line in include_str!("../../../examples/quickstart/requests.ndjson").lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        receiver
            .submit(&CaptureInput {
                payload: record["payload"].clone(),
                source: None,
                session_key: Some("example".into()),
                comparison: CaptureComparison {
                    endpoint_key: "sdk-settings".into(),
                    agent_key: None,
                    model_deployment_key: None,
                    kv_namespace: None,
                },
            })
            .unwrap();
    }
    // The typed 1 MiB override takes precedence over the file's 2 MiB capacity.
    let oversized = receiver.submit(&CaptureInput {
        payload: serde_json::json!({"model":"example", "messages":[{"role":"user", "content":"x".repeat(1024 * 1024)}]}),
        source: None,
        session_key: Some("example".into()),
        comparison: CaptureComparison {
            endpoint_key: "sdk-settings".into(), agent_key: None,
            model_deployment_key: None, kv_namespace: None,
        },
    });
    assert!(matches!(
        oversized,
        Err(SdkError::Operation { status: 4, .. })
    ));
    receiver.close().unwrap();
    assert!(settings_dir.join("override.sqlite").exists());
    assert!(!output.path().join("override.sqlite").exists());

    let mut analyzer =
        AnalyzerFactory::create(config("analyzer", output.path(), overrides())).unwrap();
    let analysis = analyzer.analyze(&Selection::default()).unwrap();
    let data: Value = serde_json::from_str(analysis.json().unwrap().get()).unwrap();
    assert_eq!(data["run"]["input_records"], 4);
    analyzer.close().unwrap();

    let mut reporter =
        ReporterFactory::create(config("reporter", output.path(), overrides())).unwrap();
    let chinese = reporter
        .render(&analysis, &RenderOptions::default())
        .unwrap();
    assert_eq!(chinese.reports().unwrap()[0].format, ReportFormat::Markdown);
    // Per-call options still outrank instance overrides.
    let english = reporter
        .render(
            &analysis,
            &RenderOptions {
                locale: Some("en-US".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_ne!(
        chinese.reports().unwrap()[0].content,
        english.reports().unwrap()[0].content
    );
    reporter.close().unwrap();
    chinese.export("report.md").unwrap();
    assert!(output.path().join("report.md").exists());

    let mut from_file =
        ReporterFactory::create(config("reporter", output.path(), Default::default())).unwrap();
    let inherited = from_file
        .render(&analysis, &RenderOptions::default())
        .unwrap();
    assert_eq!(
        inherited.reports().unwrap()[0].content,
        english.reports().unwrap()[0].content
    );
    from_file.close().unwrap();
    let saved = std::fs::read_to_string(settings).unwrap();
    assert!(saved.contains("base.sqlite"));
    assert!(!saved.contains("override.sqlite"));
    assert!(saved.contains("last_n = 1"));
    assert!(saved.contains("excluded"));
    assert!(saved.contains("max_size_bytes = 2097152"));
}

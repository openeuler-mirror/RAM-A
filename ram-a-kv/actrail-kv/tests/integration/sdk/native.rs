//! 用真实动态库验证持续持久化、按需筛选、内存报告交接及关闭后的输出所有权。
use actrail_kv_sdk::*;
use serde_json::{json, Value};
use std::{num::NonZeroUsize, path::PathBuf};

fn config(component: &str, directory: &std::path::Path, text: &str) -> SdkConfig {
    let library_dir = std::env::var_os("ACTRAIL_SDK_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        });
    SdkConfig {
        library_path: library_dir.join(format!("libactrail_{component}.so")),
        base_dir: directory.to_path_buf(),
        config_toml: Some(text.into()),
        overrides: Default::default(),
    }
}

#[test]
fn persistent_capture_to_memory_reports_and_explicit_export() {
    let directory = tempfile::tempdir().unwrap();
    let settings = "[analyzer]\noutput = 'must-not-exist.json'\n[analyzer.selection]\nlast_n = 1\n[report]\ninput = 'does-not-exist.json'\noutput = 'must-not-exist.html'\n";
    let mut receiver =
        ReceiverFactory::create(config("receiver", directory.path(), settings)).unwrap();
    for (i, line) in include_str!("../../../examples/quickstart/requests.ndjson")
        .lines()
        .enumerate()
    {
        let record: Value = serde_json::from_str(line).unwrap();
        let receipt = receiver
            .submit(&CaptureInput {
                payload: record["payload"].clone(),
                source: Some("sdk-test".into()),
                session_key: Some(format!("session-{}", i % 2)),
                comparison: CaptureComparison {
                    endpoint_key: " sdk-primary ".into(),
                    agent_key: Some("agent".into()),
                    model_deployment_key: None,
                    kv_namespace: None,
                },
            })
            .unwrap();
        assert!(receipt.captured_at.ends_with('Z'));
    }
    assert!(directory.path().join("captures.sqlite").exists());
    // 保持 receiver 存活，让按需 analyzer 读取已提交存储。
    let mut analyzer =
        AnalyzerFactory::create(config("analyzer", directory.path(), settings)).unwrap();
    let output = analyzer
        .analyze(&Selection {
            session_keys: Some(vec!["session-1".into()]),
            last_n: RecordLimit::Last(NonZeroUsize::new(1).unwrap()),
            ..Selection::default()
        })
        .unwrap();
    let data: Value = serde_json::from_str(output.json().unwrap().get()).unwrap();
    assert_eq!(data["run"]["input_records"], 1);
    let all = analyzer
        .analyze(&Selection {
            last_n: RecordLimit::All,
            ..Selection::default()
        })
        .unwrap();
    let all_data: Value = serde_json::from_str(all.json().unwrap().get()).unwrap();
    assert_eq!(all_data["run"]["input_records"], 4);
    analyzer.close().unwrap();
    receiver.close().unwrap();

    let mut reporter =
        ReporterFactory::create(config("reporter", directory.path(), settings)).unwrap();
    let english = reporter
        .render(
            &output,
            &RenderOptions {
                format: Some(ReportFormat::Html),
                locale: Some("en-US".into()),
            },
        )
        .unwrap();
    let chinese = reporter
        .render(
            &output,
            &RenderOptions {
                format: Some(ReportFormat::Html),
                locale: Some("zh-CN".into()),
            },
        )
        .unwrap();
    let english_again = reporter
        .render(
            &output,
            &RenderOptions {
                format: Some(ReportFormat::Html),
                locale: Some("en-US".into()),
            },
        )
        .unwrap();
    assert_ne!(
        english.reports().unwrap()[0].content,
        chinese.reports().unwrap()[0].content
    );
    assert_eq!(
        english.reports().unwrap()[0].content,
        english_again.reports().unwrap()[0].content
    );
    let both = reporter.render(&output, &RenderOptions::default()).unwrap();
    assert_eq!(both.reports().unwrap().len(), 2);
    reporter.close().unwrap();
    assert!(!directory.path().join("must-not-exist.json").exists());
    assert!(!directory.path().join("must-not-exist.html").exists());
    assert!(!directory.path().join("analysis.json").exists());
    assert!(!directory.path().join("report.html").exists());

    assert!(output.export("captures.sqlite").is_err());
    assert!(english.export("captures.sqlite").is_err());
    output.export("analysis.json").unwrap();
    english.export("report.html").unwrap();
    both.export_format("report.md", ReportFormat::Markdown)
        .unwrap();
    let exported: Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("analysis.json")).unwrap())
            .unwrap();
    assert_eq!(exported, data);
    assert!(exported.get("ok").is_none());
    assert!(
        std::fs::read_to_string(directory.path().join("report.html"))
            .unwrap()
            .contains("<html")
    );
    assert!(!directory.path().join("does-not-exist.json").exists());
    let cli = config("reporter", directory.path(), "")
        .library_path
        .parent()
        .unwrap()
        .join("actrail-kv-report");
    let result = std::process::Command::new(cli)
        .arg("--config")
        .arg(directory.path().join("cli.toml"))
        .arg("--input")
        .arg(directory.path().join("analysis.json"))
        .arg("--output")
        .arg(directory.path().join("cli-report.html"))
        .args(["--format", "html"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn failed_capture_does_not_poison_instance() {
    let directory = tempfile::tempdir().unwrap();
    let mut receiver = ReceiverFactory::create(config("receiver", directory.path(), "")).unwrap();
    let mut input = CaptureInput {
        payload: json!([]),
        source: None,
        session_key: None,
        comparison: CaptureComparison {
            endpoint_key: "endpoint".into(),
            agent_key: None,
            model_deployment_key: None,
            kv_namespace: None,
        },
    };
    match receiver.submit(&input) {
        Err(SdkError::Operation { status, detail }) => {
            assert_eq!(status, 3);
            assert_eq!(detail.field.as_deref(), Some("payload"));
        }
        _ => panic!("expected capture domain error"),
    }
    input.payload = json!({"model":"test","messages":[]});
    receiver.submit(&input).unwrap();
    receiver.close().unwrap();
    // 关闭释放写入锁，可再次打开同一持久存储。
    ReceiverFactory::create(config("receiver", directory.path(), ""))
        .unwrap()
        .close()
        .unwrap();
}

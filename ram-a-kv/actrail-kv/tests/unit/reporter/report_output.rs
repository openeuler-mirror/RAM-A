//! 本文件验证 HTML/Markdown 双报告选择与路径派生。

use std::{fs, path::Path};

use anyhow::Result;

use super::{ensure_distinct_paths, report_paths, write_atomic_set};
use crate::{load_result, render_html_with_locale, render_markdown_with_locale};

use crate::{locale::LOCALE_TEST_LOCK, write_report, Locale, LocaleFactory, ReportFormat};

fn fixture(directory: &std::path::Path) -> std::path::PathBuf {
    let input = directory.join("analysis.json");
    fs::write(
        &input,
        serde_json::to_vec(&crate::result_loader::tests::fixture()).expect("serialize"),
    )
    .expect("write fixture");
    input
}

#[test]
fn default_mode_generates_html_and_markdown_from_html_or_md_base() {
    for output_name in ["report.html", "report.md", "report"] {
        let directory = tempfile::tempdir().expect("tempdir");
        let input = fixture(directory.path());
        let output = directory.path().join(output_name);
        write_reports_with_locale(&input, &output, None, Locale::ZhCn).expect("write reports");

        let html = fs::read_to_string(directory.path().join("report.html")).expect("html");
        let markdown = fs::read_to_string(directory.path().join("report.md")).expect("markdown");
        assert!(html.starts_with("<!doctype html>"));
        assert!(markdown.starts_with("# LLM KV 缓存结构诊断"));

        let _guard = LOCALE_TEST_LOCK.lock().expect("locale test lock");
        LocaleFactory::set_locale(Locale::ZhCn).expect("zh locale");
        let legacy_html = directory.path().join("legacy.html");
        write_report(&input, &legacy_html).expect("legacy html");
        assert_eq!(
            fs::read(directory.path().join("report.html")).expect("html bytes"),
            fs::read(legacy_html).expect("legacy html bytes")
        );
    }
}

#[test]
fn default_mode_treats_other_extensions_as_a_base_path() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = fixture(directory.path());
    write_reports_with_locale(
        &input,
        &directory.path().join("report.v1"),
        None,
        Locale::ZhCn,
    )
    .expect("reports");

    assert!(directory.path().join("report.v1.html").exists());
    assert!(directory.path().join("report.v1.md").exists());
}

#[test]
fn legacy_html_writer_uses_global_locale_snapshot() {
    let _guard = LOCALE_TEST_LOCK.lock().expect("locale test lock");
    let directory = tempfile::tempdir().expect("tempdir");
    let input = fixture(directory.path());
    let output = directory.path().join("report.html");

    LocaleFactory::set_locale(Locale::EnUs).expect("english locale");
    write_report(&input, &output).expect("english html");
    let html = fs::read_to_string(&output).expect("html");
    LocaleFactory::set_locale(Locale::ZhCn).expect("restore locale");

    assert!(html.contains("<html lang=\"en-US\">"));
    assert!(html.contains("LLM KV Cache Structure Diagnostics"));
}

#[test]
fn explicit_format_writes_exact_output_path_only() {
    let directory = tempfile::tempdir().expect("tempdir");
    let input = fixture(directory.path());
    let html = directory.path().join("custom.out");
    write_reports_with_locale(&input, &html, Some(ReportFormat::Html), Locale::ZhCn).expect("html");
    assert!(fs::read_to_string(&html)
        .expect("html")
        .starts_with("<!doctype html>"));
    assert!(!directory.path().join("custom.md").exists());

    let markdown = directory.path().join("custom.md.out");
    write_reports_with_locale(
        &input,
        &markdown,
        Some(ReportFormat::Markdown),
        Locale::ZhCn,
    )
    .expect("markdown");
    assert!(fs::read_to_string(&markdown)
        .expect("markdown")
        .starts_with("# LLM KV 缓存结构诊断"));
    assert!(!directory.path().join("custom.md.out.html").exists());
}

fn write_reports_with_locale(
    input: &Path,
    output: &Path,
    format: Option<ReportFormat>,
    locale: crate::Locale,
) -> Result<()> {
    let snapshot = crate::LocaleSnapshot::new(locale);
    let targets = report_paths(output, format);
    for (_, path) in &targets {
        ensure_distinct_paths(input, path)?;
    }
    let result = load_result(input)?;
    let mut rendered = Vec::with_capacity(targets.len());
    for (format, path) in targets {
        let content = match format {
            ReportFormat::Html => render_html_with_locale(&result, snapshot)?,
            ReportFormat::Markdown => render_markdown_with_locale(&result, snapshot)?,
        };
        rendered.push((path, content.into_bytes()));
    }
    write_atomic_set(&rendered)
}

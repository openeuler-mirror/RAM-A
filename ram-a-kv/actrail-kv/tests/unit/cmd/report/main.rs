//! 本文件验证 main 模块的行为。

use super::{Cli, FormatArg};
use clap::Parser;

fn base() -> Vec<&'static str> {
    vec![
        "actrail-kv-report",
        "--input",
        "analysis.json",
        "--output",
        "report.html",
    ]
}

#[test]
fn format_is_optional_and_accepts_html_or_md() {
    let default = Cli::try_parse_from(base()).expect("default");
    assert_eq!(default.format, None);
    assert_eq!(default.locale, None);

    let mut html = base();
    html.extend(["--format", "html"]);
    assert_eq!(
        Cli::try_parse_from(html).expect("html").format,
        Some(FormatArg::Html)
    );

    let mut markdown = base();
    markdown.extend(["--format", "md"]);
    assert_eq!(
        Cli::try_parse_from(markdown).expect("markdown").format,
        Some(FormatArg::Md)
    );

    let mut invalid = base();
    invalid.extend(["--format", "pdf"]);
    assert!(Cli::try_parse_from(invalid).is_err());

    let mut locale = base();
    locale.extend(["--locale", "en-US"]);
    assert_eq!(
        Cli::try_parse_from(locale).expect("locale").locale,
        Some("en-US".to_string())
    );
}

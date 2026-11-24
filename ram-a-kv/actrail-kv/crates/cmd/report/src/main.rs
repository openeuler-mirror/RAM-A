//! 本文件提供 HTML/Markdown 报告 CLI 入口和退出错误输出。

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Render an Actrail KV analysis as HTML and/or Markdown"
)]
struct Cli {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Output format. Omit to generate both HTML and Markdown.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,
    /// Report locale. Omit to use zh-CN.
    #[arg(long)]
    locale: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum FormatArg {
    Html,
    Md,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let locale = cli.locale.as_deref().unwrap_or("zh-CN");
    actrail_kv_reporter::LocaleFactory::init_from_code(locale)?;
    let format = cli.format.map(|format| match format {
        FormatArg::Html => actrail_kv_reporter::ReportFormat::Html,
        FormatArg::Md => actrail_kv_reporter::ReportFormat::Markdown,
    });
    actrail_kv_reporter::write_reports(&cli.input, &cli.output, format)
}

#[cfg(test)]
#[path = "../../../../tests/unit/cmd/report/main.rs"]
mod tests;

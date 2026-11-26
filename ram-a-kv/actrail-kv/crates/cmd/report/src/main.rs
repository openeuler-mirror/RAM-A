//! 本文件提供 HTML/Markdown 报告 CLI 入口和退出错误输出。

use std::path::{self, PathBuf};

use actrail_kv_reporter::{report_paths, write_reports, LocaleFactory};
use actrail_kv_settings::ConfigFile;
use analysis_contract::settings::AnalyzerSettings;
use anyhow::Result;
use clap::Parser;
use report_contract::{ReportFormat, ReportSettings};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Render an actrail-kv analysis as HTML and/or Markdown"
)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    input: Option<PathBuf>,
    #[arg(long)]
    output: Option<PathBuf>,
    /// Output format. Omit to generate both HTML and Markdown.
    #[arg(long)]
    format: Option<ReportFormat>,
    /// Report locale. Omit to use zh-CN.
    #[arg(long)]
    locale: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut settings = ConfigFile::open(cli.config.as_deref())?;
    let file: ReportSettings = settings.read(&["report"])?;
    LocaleFactory::init_from_code(&cli.locale.unwrap_or(file.locale))?;
    let format = cli.format.or(file.format);

    let input = cli
        .input
        .or_else(|| file.input.map(|path| settings.resolve(path)));
    let input = match input {
        Some(path) => path,
        None => {
            let analyzer: AnalyzerSettings = settings.read(&["analyzer"])?;
            settings.resolve(analyzer.output)
        }
    };
    let output = match cli.output {
        Some(path) => path::absolute(path)?,
        None => {
            let default = if format == Some(ReportFormat::Markdown) {
                "report.md"
            } else {
                "report.html"
            };
            settings.resolve(file.output.unwrap_or_else(|| default.into()))
        }
    };
    for (_, path) in report_paths(&output, format) {
        settings.check_output(&path)?;
    }
    settings.finish()?;
    write_reports(&input, &output, format)
}

#[cfg(test)]
#[path = "../../../../tests/unit/cmd/report/main.rs"]
mod tests;

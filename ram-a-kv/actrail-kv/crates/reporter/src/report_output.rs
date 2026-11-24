//! 本文件编排 HTML/Markdown 报告路径、渲染和原子发布。

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use tempfile::NamedTempFile;

use crate::{load_result, render_html_with_locale, render_markdown_with_locale, LocaleFactory};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportFormat {
    Html,
    Markdown,
}

pub(crate) fn write_html_only(input: &Path, output: &Path) -> Result<()> {
    ensure_distinct_paths(input, output)?;
    let result = load_result(input)?;
    let locale = LocaleFactory::snapshot()?;
    let html = render_html_with_locale(&result, locale)?;
    write_atomic_set(&[(output.to_path_buf(), html.into_bytes())])
}

pub(crate) fn write_reports(
    input: &Path,
    output: &Path,
    format: Option<ReportFormat>,
) -> Result<()> {
    let targets = report_paths(output, format);
    for (_, path) in &targets {
        ensure_distinct_paths(input, path)?;
    }
    let result = load_result(input)?;
    let locale = LocaleFactory::snapshot()?;
    let mut rendered = Vec::with_capacity(targets.len());
    for (format, path) in targets {
        let content = match format {
            ReportFormat::Html => render_html_with_locale(&result, locale)?,
            ReportFormat::Markdown => render_markdown_with_locale(&result, locale)?,
        };
        rendered.push((path, content.into_bytes()));
    }
    write_atomic_set(&rendered)
}

fn report_paths(output: &Path, format: Option<ReportFormat>) -> Vec<(ReportFormat, PathBuf)> {
    if let Some(format) = format {
        return vec![(format, output.to_path_buf())];
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let file_name = output
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("report"));
    let file_name = file_name.to_string_lossy();
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let base = if extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("md") {
        Path::new(file_name.as_ref())
            .file_stem()
            .unwrap_or_else(|| std::ffi::OsStr::new("report"))
            .to_string_lossy()
            .into_owned()
    } else {
        file_name.into_owned()
    };
    vec![
        (ReportFormat::Html, parent.join(format!("{base}.html"))),
        (ReportFormat::Markdown, parent.join(format!("{base}.md"))),
    ]
}

fn write_atomic_set(outputs: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    let mut staged = Vec::with_capacity(outputs.len());
    for (path, bytes) in outputs {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
            format!("failed to create temporary report in {}", parent.display())
        })?;
        temporary.write_all(bytes)?;
        temporary.as_file_mut().sync_all()?;
        staged.push((temporary, path.clone(), parent.to_path_buf()));
    }
    let parents: Vec<PathBuf> = staged.iter().map(|entry| entry.2.clone()).collect();
    for (temporary, path, _) in staged {
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to publish report {}", path.display()))?;
    }
    let mut synced = Vec::new();
    for parent in parents {
        if !synced.contains(&parent) {
            sync_parent(&parent)?;
            synced.push(parent);
        }
    }
    Ok(())
}

fn ensure_distinct_paths(input: &Path, output: &Path) -> Result<()> {
    if output.exists() && same_file::is_same_file(input, output).unwrap_or(false) {
        bail!("input and output must refer to different files");
    }
    if !output.exists() {
        let input = input.canonicalize()?;
        let parent = output
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?;
        let destination = match output.file_name() {
            Some(name) => parent.join(name),
            None => parent,
        };
        if input == destination {
            bail!("input and output must refer to different files");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/reporter/report_output.rs"]
mod tests;

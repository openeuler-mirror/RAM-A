// Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
//
// RAM-A is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

mod chunker;
pub mod error;
pub mod ingestor;
mod llm;
pub mod model;
mod parser;
mod repo;
pub mod routes;
pub mod service;
mod token_counter;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use memory_core::{
    EmbeddingProvider, HashEmbedding, MemoryManager, OpenRouterEmbedding, RetrievalConfig,
    SearchMode, SqliteMemoryStore,
};

use crate::llm::DocumentSummaryClient;
use crate::model::{CreateDatasetRequest, CreateDocumentFileRequest};
use crate::repo::RagRepository;
use crate::service::{RagConfig, RagService};
use crate::token_counter::{Cl100kTokenCounter, TokenCounter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingProviderKind {
    Hash,
    OpenAiCompatible,
}

#[derive(Clone, Debug)]
pub struct CaseServiceOptions {
    pub rag_store: PathBuf,
    pub memory_store: PathBuf,
    pub embedding_provider: EmbeddingProviderKind,
    pub embedding_api_key_env: String,
    pub embedding_base_url: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub chunk_size: usize,
    pub summary_llm_model: Option<String>,
    pub summary_llm_api_key_env: String,
    pub summary_llm_base_url: String,
    pub summary_llm_timeout_ms: u64,
}

/// Returns whether an operation failed because the case business database file is missing.
pub fn is_business_database_missing(error: &anyhow::Error) -> bool {
    repo::is_business_database_missing(error)
}

/// Returns whether an operation failed because the case index database file is missing.
pub fn is_case_index_database_missing(error: &anyhow::Error) -> bool {
    service::is_case_index_database_missing(error)
}

/// Returns whether an administrator-requested index rebuild is already active.
pub fn is_case_index_rebuild_in_progress(error: &anyhow::Error) -> bool {
    service::is_case_index_rebuild_in_progress(error)
}

pub fn build_service(options: &CaseServiceOptions) -> Result<Arc<RagService>> {
    let repo = Arc::new(RagRepository::new(&options.rag_store));
    repo.initialize()?;

    let store = Arc::new(SqliteMemoryStore::new_existing(&options.memory_store));
    let index_exists = options.memory_store.try_exists().with_context(|| {
        format!(
            "failed to inspect case index database {}",
            options.memory_store.display()
        )
    })?;
    if !index_exists && !repo.has_indexable_chunks()? {
        store
            .initialize_blocking()
            .context("failed to initialize empty case index database during daemon startup")?;
    }
    let embedder = build_embedding_provider(options)?;
    let retrieval = RetrievalConfig {
        mode: SearchMode::Hybrid,
        ..RetrievalConfig::default()
    };
    let memory = Arc::new(MemoryManager::with_retrieval_config(
        store.clone(),
        embedder.clone(),
        retrieval,
    ));
    let file_root = options
        .rag_store
        .parent()
        .map(|path| path.join("memory-cases-files"))
        .unwrap_or_else(|| "memory-cases-files".into());

    Ok(Arc::new(RagService::new(
        repo,
        memory,
        embedder,
        store,
        RagConfig {
            file_root,
            chunk_size: options.chunk_size,
            token_counter: build_chunk_token_counter()?,
            summary_llm: build_summary_llm(options)?,
        },
    )))
}

pub async fn import_documents_from_dir(
    service: &RagService,
    dataset_id: &str,
    source_dir: &Path,
) -> Result<usize> {
    anyhow::ensure!(
        source_dir.is_dir(),
        "case library source_dir does not exist or is not a directory: {}",
        source_dir.display()
    );
    ensure_dataset(service, dataset_id)?;

    let existing_names = service
        .list_documents(dataset_id)?
        .documents
        .into_iter()
        .map(|document| document.name)
        .collect::<std::collections::HashSet<_>>();
    let mut imported = 0usize;
    for source_document in source_documents_from_dir(source_dir)? {
        if existing_names.contains(&source_document.name) {
            continue;
        }
        let file_name = source_document
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| {
                format!(
                    "case document path is not valid UTF-8: {}",
                    source_document.path.display()
                )
            })?
            .to_string();
        let bytes = tokio::fs::read(&source_document.path)
            .await
            .with_context(|| {
                format!(
                    "failed to read case document {}",
                    source_document.path.display()
                )
            })?;
        service
            .create_document(
                dataset_id,
                CreateDocumentFileRequest {
                    id: None,
                    task_id: None,
                    name: source_document.name,
                    file_name,
                    mime_type: mime_type_for_file(&source_document.path).map(str::to_string),
                    bytes,
                },
            )
            .await
            .with_context(|| {
                format!(
                    "failed to create case document {}",
                    source_document.path.display()
                )
            })?;
        imported = imported.saturating_add(1);
    }

    Ok(imported)
}

#[derive(Debug, Eq, PartialEq)]
struct SourceDocument {
    path: PathBuf,
    name: String,
}

fn source_documents_from_dir(source_dir: &Path) -> Result<Vec<SourceDocument>> {
    source_documents_from_dir_with_reader(source_dir, |directory| std::fs::read_dir(directory))
}

fn source_documents_from_dir_with_reader<I>(
    source_dir: &Path,
    mut read_dir: impl FnMut(&Path) -> std::io::Result<I>,
) -> Result<Vec<SourceDocument>>
where
    I: Iterator<Item = std::io::Result<std::fs::DirEntry>>,
{
    let mut documents = Vec::new();
    let mut pending_directories = vec![source_dir.to_path_buf()];

    while let Some(directory) = pending_directories.pop() {
        let entries = match read_dir(&directory)
            .and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>())
        {
            Ok(entries) => entries,
            Err(error) if directory == source_dir => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to read case library directory {}",
                        directory.display()
                    )
                });
            }
            Err(error) => {
                tracing::warn!(
                    event = "ram_a.case.ingestion.degraded",
                    stage = "source_scan",
                    action = "skip_directory",
                    directory = %directory.display(),
                    error_kind = ?error.kind(),
                    error = %error
                );
                continue;
            }
        };

        for entry in entries {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    tracing::warn!(
                        event = "ram_a.case.ingestion.degraded",
                        stage = "source_scan",
                        action = "skip_entry",
                        path = %path.display(),
                        error_kind = ?error.kind(),
                        error = %error
                    );
                    continue;
                }
            };
            if file_type.is_dir() {
                pending_directories.push(path);
                continue;
            }
            if !file_type.is_file() || !is_supported_document(&path) {
                continue;
            }

            let relative_path = path.strip_prefix(source_dir).with_context(|| {
                format!(
                    "case document {} is outside source_dir {}",
                    path.display(),
                    source_dir.display()
                )
            })?;
            let name = relative_document_name(relative_path)?;
            documents.push(SourceDocument { path, name });
        }
    }

    documents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(documents)
}

fn relative_document_name(relative_path: &Path) -> Result<String> {
    let mut components = Vec::new();
    for component in relative_path.components() {
        let std::path::Component::Normal(component) = component else {
            anyhow::bail!(
                "case document relative path contains an unsupported component: {}",
                relative_path.display()
            );
        };
        components.push(
            component
                .to_str()
                .with_context(|| {
                    format!(
                        "case document relative path is not valid UTF-8: {}",
                        relative_path.display()
                    )
                })?
                .to_string(),
        );
    }
    anyhow::ensure!(
        !components.is_empty(),
        "case document relative path must not be empty"
    );
    Ok(components.join("/"))
}

fn ensure_dataset(service: &RagService, dataset_id: &str) -> Result<()> {
    if service
        .list_datasets()?
        .datasets
        .iter()
        .any(|dataset| dataset.id == dataset_id)
    {
        return Ok(());
    }
    service.create_dataset(CreateDatasetRequest {
        id: Some(dataset_id.to_string()),
        name: dataset_id.to_string(),
        description: Some("RAM-A configured case library".to_string()),
    })?;
    Ok(())
}

fn build_embedding_provider(options: &CaseServiceOptions) -> Result<Arc<dyn EmbeddingProvider>> {
    match options.embedding_provider {
        EmbeddingProviderKind::Hash => {
            Ok(Arc::new(HashEmbedding::new(options.embedding_dimensions)))
        }
        EmbeddingProviderKind::OpenAiCompatible => {
            let api_key = std::env::var(&options.embedding_api_key_env).map_err(|_| {
                anyhow::anyhow!(
                    "embedding API key environment variable `{}` is unavailable",
                    options.embedding_api_key_env
                )
            })?;
            anyhow::ensure!(
                !api_key.trim().is_empty(),
                "embedding API key environment variable `{}` is empty",
                options.embedding_api_key_env
            );
            anyhow::ensure!(
                !options.embedding_base_url.trim().is_empty(),
                "embedding base URL is empty"
            );
            anyhow::ensure!(
                !options.embedding_model.trim().is_empty(),
                "embedding model is empty"
            );
            Ok(Arc::new(OpenRouterEmbedding::with_base_url(
                api_key,
                &options.embedding_base_url,
                &options.embedding_model,
                options.embedding_dimensions,
            )))
        }
    }
}

fn build_chunk_token_counter() -> Result<Arc<dyn TokenCounter>> {
    let counter = Cl100kTokenCounter::new()?;
    eprintln!("memory-cases chunk tokenizer: {}", counter.name());
    Ok(Arc::new(counter))
}

fn build_summary_llm(options: &CaseServiceOptions) -> Result<Option<DocumentSummaryClient>> {
    let Some(model) = options.summary_llm_model.as_deref() else {
        return Ok(None);
    };
    let api_key = match std::env::var(&options.summary_llm_api_key_env) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!(
                "summary LLM disabled: {} is not set; using offline document summary",
                options.summary_llm_api_key_env
            );
            return Ok(None);
        }
    };

    Ok(Some(DocumentSummaryClient::new(
        api_key,
        &options.summary_llm_base_url,
        model,
        Duration::from_millis(options.summary_llm_timeout_ms),
    )))
}

fn is_supported_document(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdx" | "txt" | "text" | "log"
            )
        })
        .unwrap_or(false)
}

fn mime_type_for_file(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" | "mdx" => Some("text/markdown"),
        "txt" | "text" | "log" => Some("text/plain"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::io;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{
        build_service, import_documents_from_dir, source_documents_from_dir,
        source_documents_from_dir_with_reader, CaseServiceOptions, EmbeddingProviderKind,
    };

    fn write_source_file(source_dir: &Path, relative_path: &str, contents: &str) {
        let path = source_dir.join(relative_path);
        fs::create_dir_all(path.parent().expect("source file parent"))
            .expect("create source directory");
        fs::write(path, contents).expect("write source file");
    }

    fn source_document_names(source_dir: &Path) -> Vec<String> {
        source_documents_from_dir(source_dir)
            .expect("scan source documents")
            .into_iter()
            .map(|document| document.name)
            .collect()
    }

    fn test_service(temp: &TempDir) -> std::sync::Arc<crate::service::RagService> {
        build_service(&CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: temp.path().join("case-index.sqlite"),
            embedding_provider: EmbeddingProviderKind::Hash,
            embedding_api_key_env: "UNUSED_CASE_EMBEDDING_KEY".to_string(),
            embedding_base_url: "http://127.0.0.1:1/v1".to_string(),
            embedding_model: "hash".to_string(),
            embedding_dimensions: 32,
            chunk_size: 32,
            summary_llm_model: None,
            summary_llm_api_key_env: "UNUSED_CASE_SUMMARY_KEY".to_string(),
            summary_llm_base_url: "http://127.0.0.1:1/v1".to_string(),
            summary_llm_timeout_ms: 1_000,
        })
        .expect("build case service")
    }

    #[test]
    fn source_scan_keeps_same_file_names_from_different_directories() {
        let temp = TempDir::new().expect("create temp directory");
        let source_dir = temp.path().join("cases");
        write_source_file(&source_dir, "network/incident.md", "network incident");
        write_source_file(&source_dir, "storage/incident.md", "storage incident");

        assert_eq!(
            source_document_names(&source_dir),
            vec!["network/incident.md", "storage/incident.md"]
        );
    }

    #[test]
    fn source_scan_recurses_into_nested_directories() {
        let temp = TempDir::new().expect("create temp directory");
        let source_dir = temp.path().join("cases");
        write_source_file(
            &source_dir,
            "security/authentication/mfa/timeout.txt",
            "nested case",
        );

        assert_eq!(
            source_document_names(&source_dir),
            vec!["security/authentication/mfa/timeout.txt"]
        );
    }

    #[test]
    fn source_scan_skips_unsupported_file_types() {
        let temp = TempDir::new().expect("create temp directory");
        let source_dir = temp.path().join("cases");
        write_source_file(&source_dir, "supported/case.log", "supported case");
        write_source_file(&source_dir, "unsupported/case.json", "{}");
        write_source_file(&source_dir, "unsupported/screenshot.png", "not an image");

        assert_eq!(
            source_document_names(&source_dir),
            vec!["supported/case.log"]
        );
    }

    #[test]
    fn source_scan_skips_unreadable_or_disappearing_subdirectories() {
        for error_kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::NotFound] {
            let temp = TempDir::new().expect("create temp directory");
            let source_dir = temp.path().join("cases");
            write_source_file(&source_dir, "root.md", "root case");
            write_source_file(&source_dir, "readable/nested/case.txt", "readable case");
            let unavailable_dir = source_dir.join("unavailable");
            fs::create_dir(&unavailable_dir).expect("create unavailable directory");
            let mut encountered_failure = false;

            let documents = source_documents_from_dir_with_reader(&source_dir, |directory| {
                if directory == unavailable_dir {
                    encountered_failure = true;
                    if error_kind == io::ErrorKind::NotFound {
                        fs::remove_dir(directory).expect("remove directory during scan");
                    } else {
                        // chmod(000) is still readable by root, so inject EACCES directly.
                        return Err(io::Error::from(error_kind));
                    }
                }
                fs::read_dir(directory)
            })
            .expect("skip the unavailable subtree and continue scanning");

            assert!(encountered_failure, "the failing directory must be visited");
            let names = documents
                .into_iter()
                .map(|document| document.name)
                .collect::<Vec<_>>();
            assert_eq!(names, vec!["readable/nested/case.txt", "root.md"]);
        }
    }

    #[test]
    fn source_scan_skips_subdirectories_with_entry_enumeration_errors() {
        let temp = TempDir::new().expect("create temp directory");
        let source_dir = temp.path().join("cases");
        write_source_file(&source_dir, "readable/case.md", "readable case");
        write_source_file(&source_dir, "interrupted/partial.md", "partial case");
        let interrupted_dir = source_dir.join("interrupted");
        let mut encountered_failure = false;

        let documents = source_documents_from_dir_with_reader(&source_dir, |directory| {
            let mut entries = fs::read_dir(directory)?.collect::<Vec<_>>();
            if directory == interrupted_dir {
                encountered_failure = true;
                entries.push(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
            }
            Ok(entries.into_iter())
        })
        .expect("skip a partially enumerated subtree and continue scanning");

        assert!(encountered_failure, "the failing directory must be visited");
        let names = documents
            .into_iter()
            .map(|document| document.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["readable/case.md"]);
    }

    #[test]
    fn source_scan_preserves_root_directory_errors() {
        let temp = TempDir::new().expect("create temp directory");
        let missing_dir = temp.path().join("missing");
        let error = source_documents_from_dir(&missing_dir).expect_err("root must be readable");
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert!(error.to_string().contains(&missing_dir.display().to_string()));

        let source_dir = temp.path().join("cases");
        write_source_file(&source_dir, "case.md", "root case");
        let error = source_documents_from_dir_with_reader(&source_dir, |_| {
            Err::<fs::ReadDir, _>(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .expect_err("root open errors must not become successful empty scans");
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );

        let error = source_documents_from_dir_with_reader(&source_dir, |directory| {
            let mut entries = fs::read_dir(directory)?.collect::<Vec<_>>();
            entries.push(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
            Ok(entries.into_iter())
        })
        .expect_err("root enumeration errors must not become successful empty scans");
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn recursive_import_uses_relative_names_and_is_idempotent() {
        let temp = TempDir::new().expect("create temp directory");
        let source_dir = temp.path().join("cases");
        write_source_file(&source_dir, "network/incident.md", "network incident");
        write_source_file(&source_dir, "storage/incident.md", "storage incident");
        let service = test_service(&temp);

        assert_eq!(
            import_documents_from_dir(&service, "dataset-1", &source_dir)
                .await
                .expect("import source documents"),
            2
        );
        let names = service
            .list_documents("dataset-1")
            .expect("list imported documents")
            .documents
            .into_iter()
            .map(|document| document.name)
            .collect::<HashSet<_>>();
        assert_eq!(
            names,
            HashSet::from([
                "network/incident.md".to_string(),
                "storage/incident.md".to_string(),
            ])
        );
        assert_eq!(
            import_documents_from_dir(&service, "dataset-1", &source_dir)
                .await
                .expect("repeat source import"),
            0
        );
    }
}

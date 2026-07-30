use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{Multipart, Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::model::{
    ChatCompletionRequest, CreateDatasetRequest, CreateDocumentFileRequest, SearchRequest,
    UpdateDocumentFileRequest,
};
use crate::service::RagService;

pub async fn serve(bind: SocketAddr, service: Arc<RagService>) -> Result<()> {
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/datasets", post(create_dataset).get(list_datasets))
        .route(
            "/api/v1/datasets/:dataset_id/documents",
            post(create_document).get(list_documents),
        )
        .route(
            "/api/v1/datasets/:dataset_id/documents/:document_id",
            put(update_document).delete(delete_document),
        )
        .route("/api/v1/tasks/:task_id", get(get_task))
        .route(
            "/api/v1/datasets/:dataset_id/documents/:document_id/chunks",
            get(list_chunks),
        )
        .route("/api/v1/datasets/:dataset_id/search", post(search_dataset))
        .route("/api/v1/chat/completions", post(chat_completion))
        .with_state(service);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("memory-cases API listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_dataset(
    State(service): State<Arc<RagService>>,
    Json(request): Json<CreateDatasetRequest>,
) -> AppResult<Json<crate::model::Dataset>> {
    service
        .create_dataset(request)
        .map(Json)
        .map_err(map_service_error)
}

async fn list_datasets(
    State(service): State<Arc<RagService>>,
) -> AppResult<Json<crate::model::ListDatasetsResponse>> {
    service
        .list_datasets()
        .map(Json)
        .map_err(AppError::internal)
}

async fn create_document(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
    multipart: Multipart,
) -> AppResult<Json<crate::model::CreateDocumentResponse>> {
    let request = parse_document_upload(multipart).await?;
    service
        .create_document(&dataset_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn list_documents(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
) -> AppResult<Json<crate::model::ListDocumentsResponse>> {
    service
        .list_documents(&dataset_id)
        .map(Json)
        .map_err(map_service_error)
}

async fn update_document(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
    multipart: Multipart,
) -> AppResult<Json<crate::model::UpdateDocumentResponse>> {
    let request = parse_document_update(multipart).await?;
    service
        .update_document(&dataset_id, &document_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn delete_document(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
) -> AppResult<Json<crate::model::DeleteDocumentResponse>> {
    service
        .delete_document(&dataset_id, &document_id)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn get_task(
    State(service): State<Arc<RagService>>,
    Path(task_id): Path<String>,
) -> AppResult<Json<crate::model::IngestionTask>> {
    let task = service.get_task(&task_id).map_err(AppError::internal)?;
    task.map(Json)
        .ok_or_else(|| AppError::not_found("task not found"))
}

async fn list_chunks(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
) -> AppResult<Json<crate::model::ListChunksResponse>> {
    service
        .list_chunks(&dataset_id, &document_id)
        .map(Json)
        .map_err(map_service_error)
}

async fn search_dataset(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
    Json(request): Json<SearchRequest>,
) -> AppResult<Json<crate::model::SearchResponse>> {
    service
        .search_dataset(&dataset_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn chat_completion(
    State(service): State<Arc<RagService>>,
    Json(request): Json<ChatCompletionRequest>,
) -> AppResult<Json<crate::model::ChatCompletionResponse>> {
    service
        .chat_completion(request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

fn map_service_error(error: anyhow::Error) -> AppError {
    let message = error.to_string();
    if message.contains("not found") {
        AppError::not_found(message)
    } else if message.contains("must")
        || message.contains("requires")
        || message.contains("required")
    {
        AppError::bad_request(message)
    } else {
        AppError::internal(message)
    }
}

async fn parse_document_upload(mut multipart: Multipart) -> AppResult<CreateDocumentFileRequest> {
    let mut id = None;
    let mut task_id = None;
    let mut name = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(AppError::internal)? {
        match field.name().unwrap_or_default() {
            "id" => id = Some(field.text().await.map_err(AppError::internal)?),
            "task_id" => task_id = Some(field.text().await.map_err(AppError::internal)?),
            "name" => name = Some(field.text().await.map_err(AppError::internal)?),
            "file" => {
                file_name = field.file_name().map(str::to_string);
                mime_type = field.content_type().map(str::to_string);
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(AppError::internal)?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let file_name = file_name.ok_or_else(|| AppError::bad_request("file is required"))?;
    Ok(CreateDocumentFileRequest {
        id,
        task_id,
        name: name.unwrap_or_else(|| file_name.clone()),
        file_name,
        mime_type,
        bytes: bytes.ok_or_else(|| AppError::bad_request("file is required"))?,
    })
}

async fn parse_document_update(mut multipart: Multipart) -> AppResult<UpdateDocumentFileRequest> {
    let mut task_id = None;
    let mut name = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(AppError::internal)? {
        match field.name().unwrap_or_default() {
            "task_id" => task_id = Some(field.text().await.map_err(AppError::internal)?),
            "name" => name = Some(field.text().await.map_err(AppError::internal)?),
            "file" => {
                file_name = field.file_name().map(str::to_string);
                mime_type = field.content_type().map(str::to_string);
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(AppError::internal)?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    Ok(UpdateDocumentFileRequest {
        task_id,
        name,
        file_name,
        mime_type,
        bytes,
    })
}

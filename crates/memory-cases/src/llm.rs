use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 512;

#[derive(Clone)]
pub struct DocumentSummaryClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    timeout: Duration,
}

impl DocumentSummaryClient {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            timeout,
        }
    }

    pub async fn summarize_document(&self, document_name: &str, content: &str) -> Result<String> {
        let response = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth(&self.api_key)
            .timeout(self.timeout)
            .json(&summary_request_body(&self.model, document_name, content))
            .send()
            .await
            .context("document summary LLM request failed")?;

        let status = response.status();
        let body_text = response
            .text()
            .await
            .context("failed to read document summary LLM response body")?;
        if !status.is_success() {
            anyhow::bail!(
                "document summary LLM returned HTTP {status}: {}",
                preview_body(&body_text)
            );
        }

        let body: ChatCompletionResponse =
            serde_json::from_str(&body_text).with_context(|| {
                format!(
                    "failed to decode document summary LLM response: {}",
                    preview_body(&body_text)
                )
            })?;
        let summary = body
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .unwrap_or_default();
        let summary = clean_summary_text(&summary);
        anyhow::ensure!(!summary.is_empty(), "document summary LLM returned empty content");
        Ok(summary)
    }

    fn chat_completions_url(&self) -> String {
        if self.base_url.ends_with("/chat/completions") {
            self.base_url.clone()
        } else {
            format!("{}/chat/completions", self.base_url)
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

fn summary_request_body<'a>(
    model: &'a str,
    document_name: &str,
    content: &str,
) -> ChatCompletionRequest<'a> {
    ChatCompletionRequest {
        model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: "你是文档检索系统的摘要器。请只根据输入文档生成用于检索召回的摘要，不要编造。优先保留错误码、命令、服务名、产品名、英文缩写、日志短语、根因和处理动作。输出纯文本，不要 Markdown。"
                    .to_string(),
            },
            ChatMessage {
                role: "user",
                content: format!("文档名: {document_name}\n\n文档内容:\n{content}"),
            },
        ],
        temperature: 0.0,
        max_tokens: SUMMARY_MAX_OUTPUT_TOKENS,
    }
}

fn clean_summary_text(summary: &str) -> String {
    summary
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn preview_body(body: &str) -> String {
    const MAX_BODY_PREVIEW_CHARS: usize = 300;
    let mut preview = body.chars().take(MAX_BODY_PREVIEW_CHARS).collect::<String>();
    if body.chars().count() > MAX_BODY_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_url_accepts_base_or_full_endpoint() {
        let client = DocumentSummaryClient::new(
            "key",
            "https://example.test/v1",
            "model",
            Duration::from_secs(1),
        );
        assert_eq!(
            client.chat_completions_url(),
            "https://example.test/v1/chat/completions"
        );

        let client = DocumentSummaryClient::new(
            "key",
            "https://example.test/v1/chat/completions",
            "model",
            Duration::from_secs(1),
        );
        assert_eq!(
            client.chat_completions_url(),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn summary_request_body_preserves_retrieval_signals() {
        let body = summary_request_body(
            "summary-model",
            "D17-网关日志-错误码.log",
            "tls handshake timeout while connecting to upstream payment-api",
        );

        assert_eq!(body.model, "summary-model");
        assert!(body.messages[0].content.contains("错误码"));
        assert!(body.messages[0].content.contains("日志短语"));
        assert!(body.messages[1].content.contains("payment-api"));
    }

    #[test]
    fn clean_summary_text_removes_blank_lines() {
        assert_eq!(clean_summary_text("  第一行  \n\n 第二行\n"), "第一行\n第二行");
    }

}

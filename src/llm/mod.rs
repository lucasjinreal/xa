use crate::config::Config;
use openai_api_rs::v1::api::OpenAIClient;
use openai_api_rs::v1::chat_completion::{self, Content, MessageRole};
use openai_api_rs::v1::chat_completion::chat_completion::ChatCompletionRequest;
use openai_api_rs::v1::chat_completion::chat_completion_stream::{ChatCompletionStreamRequest, ChatCompletionStreamResponse};
use openai_api_rs::v1::error::APIError;
use tokio_stream::StreamExt;
use std::io::Write;
use tokio::time::{Instant, sleep, Duration};

const MAX_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY_MS: u64 = 500;
const MAX_RETRY_DELAY_MS: u64 = 10_000;

/// Returns true if the error is transient and worth retrying.
/// We retry on connection errors, timeouts, and 5xx server errors.
fn is_retryable_error(err: &APIError) -> bool {
    match err {
        APIError::ReqwestError(req_err) => {
            // Connection refused / DNS failure / TLS handshake failure
            if req_err.is_connect() {
                return true;
            }
            // Request timeout
            if req_err.is_timeout() {
                return true;
            }
            // Server-side errors (5xx) — may be transient
            if let Some(status) = req_err.status() {
                if status.is_server_error() {
                    return true;
                }
            }
            false
        }
        APIError::CustomError { message } => {
            // If the custom error message mentions connectivity, treat as retryable
            let msg = message.to_lowercase();
            msg.contains("connection")
                || msg.contains("timeout")
                || msg.contains("retry")
                || msg.contains("502")
                || msg.contains("503")
                || msg.contains("504")
        }
    }
}

/// Calculates the delay for a given retry attempt using exponential backoff.
fn retry_delay(attempt: u32) -> Duration {
    // Exponential: 500ms, 1s, 2s, ... capped at MAX_RETRY_DELAY_MS
    let base = INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt.saturating_sub(1));
    Duration::from_millis(base.min(MAX_RETRY_DELAY_MS))
}

pub async fn process_with_llm(config: &Config, prompt: &str, stream: bool) -> Result<String, Box<dyn std::error::Error>> {
    // Determine if we're using OpenRouter or OpenAI based on the base_url
    let api_key = config.api_key.clone();
    let mut client = OpenAIClient::builder()
        .with_api_key(api_key);

    // Set custom base URL if needed (for OpenRouter or other OpenAI-compatible APIs)
    if !config.base_url.is_empty() && config.base_url != "https://api.openai.com/v1" {
        client = client.with_endpoint(&config.base_url);
    }

    let client = client.build()?;

    let model = config.default_model.as_deref().unwrap_or("gpt-4o-mini").to_string();

    if stream {
        // Streaming mode
        let start_time = Instant::now();

        let req = ChatCompletionStreamRequest::new(
            model,
            vec![chat_completion::ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(prompt.to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
        );

        let mut stream = retry_with_backoff("stream", || {
            let client = &client;
            let req = req.clone();
            async { client.chat_completion_stream(req).await }
        }).await?;

        let mut full_response = String::new();

        while let Some(result) = stream.next().await {
            match result {
                ChatCompletionStreamResponse::Content(content) => {
                    // Only print if content is not empty to avoid printing artifacts like >>>>>>>>
                    if !content.is_empty() {
                        print!("{}", content);
                        std::io::stdout().flush()?;
                        full_response.push_str(&content);
                    }
                }
                // v10 surfaces reasoning separately. Legacy prompt mode only
                // returns user-visible completion text, so keep it out of the
                // rendered response just as providers that embed reasoning do.
                ChatCompletionStreamResponse::Reasoning(_) => {}
                ChatCompletionStreamResponse::ToolCall(tool_calls) => {
                    // Handle tool calls if needed
                    eprintln!("Tool call received: {:?}", tool_calls);
                }
                ChatCompletionStreamResponse::Done => {
                    // Stream completed
                    break;
                }
            }
        }

        let duration = start_time.elapsed();
        // Only print timing info if we actually received content
        if !full_response.trim().is_empty() {
            println!("\n\n(Completed in {:.2?})", duration);
        }

        Ok(full_response)
    } else {
        // Non-streaming mode
        println!("Processing...");

        let start_time = Instant::now();

        let req = ChatCompletionRequest::new(
            model,
            vec![chat_completion::ChatCompletionMessage {
                role: MessageRole::user,
                content: Content::Text(prompt.to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
        );

        let result = retry_with_backoff("non-stream", || {
            let client = &client;
            let req = req.clone();
            async { client.chat_completion(req).await }
        }).await?;

        let content = if !result.inner.choices.is_empty() {
            if let Some(content) = &result.inner.choices[0].message.content {
                content.clone()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let duration = start_time.elapsed();
        println!("\n(Completed in {:.2?})", duration);

        Ok(content)
    }
}

/// Generic retry wrapper: runs `op`, and on retryable errors waits with exponential
/// backoff and retries up to `MAX_RETRIES` times before returning the last error.
async fn retry_with_backoff<F, Fut, T>(label: &str, op: F) -> Result<T, Box<dyn std::error::Error>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, APIError>>,
{
    let mut last_err: Option<APIError> = None;

    for attempt in 0..=MAX_RETRIES {
        match op().await {
            Ok(val) => return Ok(val),
            Err(err) => {
                if !is_retryable_error(&err) || attempt >= MAX_RETRIES {
                    return Err(Box::new(err));
                }
                let delay = retry_delay(attempt + 1);
                let err_display = format!("{err}");
                eprintln!(
                    "[xa] {} request failed (attempt {}/{}): {err_display}. Retrying in {:.1}s...",
                    label,
                    attempt + 1,
                    MAX_RETRIES,
                    delay.as_secs_f64()
                );
                last_err = Some(err);
                sleep(delay).await;
            }
        }
    }

    // Should not reach here, but just in case
    Err(Box::new(last_err.unwrap()))
}

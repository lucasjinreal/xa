use crate::config::Config;
use openai_api_rs::v1::api::OpenAIClient;
use openai_api_rs::v1::chat_completion::{self, Content, MessageRole};
use openai_api_rs::v1::chat_completion::chat_completion::ChatCompletionRequest;
use openai_api_rs::v1::chat_completion::chat_completion_stream::{ChatCompletionStreamRequest, ChatCompletionStreamResponse};
use tokio_stream::StreamExt;
use std::io::Write;
use tokio::time::Instant;

pub async fn process_with_llm(config: &Config, prompt: &str, stream: bool) -> Result<String, Box<dyn std::error::Error>> {
    // Determine if we're using OpenRouter or OpenAI based on the base_url
    let api_key = config.api_key.clone();
    let mut client = OpenAIClient::builder()
        .with_api_key(api_key);

    // Set custom base URL if needed (for OpenRouter or other OpenAI-compatible APIs)
    if !config.base_url.is_empty() && config.base_url != "https://api.openai.com/v1" {
        client = client.with_endpoint(&config.base_url);
    }

    let mut client = client.build()?;

    let model = config.default_model.as_deref().unwrap_or("gpt-4o-mini").to_string();

    if stream {
        // Streaming mode
        // Don't print "Processing..." in interactive mode to avoid clutter

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

        let mut stream = client.chat_completion_stream(req).await?;

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

        let result = client.chat_completion(req).await?;

        let content = if !result.choices.is_empty() {
            if let Some(content) = &result.choices[0].message.content {
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
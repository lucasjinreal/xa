use crate::config::Config;
use reqwest;
use serde_json::json;
use tokio::time::Instant;
use tokio_stream::StreamExt;
use std::io::Write;


#[derive(serde::Deserialize)]
struct NonStreamChoice {
    message: Message,
}

#[derive(serde::Deserialize)]
struct Message {
    content: String,
}

#[derive(serde::Deserialize)]
struct NonStreamResponse {
    choices: Vec<NonStreamChoice>,
}

pub async fn process_with_llm(config: &Config, prompt: &str, stream: bool) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let model = config.default_model.as_deref().unwrap_or("gpt-4o-mini");

    if stream {
        // Streaming mode
        // Don't print "Processing..." in interactive mode to avoid clutter

        let start_time = Instant::now();

        let response = client
            .post(config.base_url.replace("/v1", "") + "/v1/chat/completions") // Ensure correct endpoint
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ],
                "stream": true,
                "temperature": 0.7
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(format!("Error calling LLM API: {}", error_text).into());
        }

        let mut stream = response.bytes_stream();
        let mut full_response = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let text = String::from_utf8_lossy(&chunk);

            // Handle the SSE format
            for line in text.lines() {
                if line.starts_with("data: ") {
                    let data = &line[6..]; // Remove "data: " prefix
                    if data == "[DONE]" {
                        break;
                    }

                    if let Ok(stream_response) = serde_json::from_str::<serde_json::Value>(data) {
                        if let Some(choices) = stream_response["choices"].as_array() {
                            for choice in choices {
                                if let Some(delta) = choice["delta"].as_object() {
                                    if let Some(content) = delta["content"].as_str() {
                                        // Only print if content is not empty to avoid printing artifacts like >>>>>>>>
                                        if !content.is_empty() {
                                            print!("{}", content);
                                            std::io::stdout().flush()?;
                                            full_response.push_str(content);
                                        }
                                    }
                                }
                            }
                        }
                    }
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

        let response = client
            .post(config.base_url.replace("/v1", "") + "/v1/chat/completions") // Ensure correct endpoint
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ],
                "temperature": 0.7
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(format!("Error calling LLM API: {}", error_text).into());
        }

        let openai_response: NonStreamResponse = response.json().await?;

        let result = if let Some(choice) = openai_response.choices.first() {
            choice.message.content.clone()
        } else {
            String::new()
        };

        let duration = start_time.elapsed();
        println!("\n(Completed in {:.2?})", duration);

        Ok(result)
    }
}
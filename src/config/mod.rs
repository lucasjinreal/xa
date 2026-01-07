use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use dirs::config_dir;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub default_model: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "".to_string(),
            default_model: Some("gpt-4o-mini".to_string()),
        }
    }
}

use reqwest;

#[derive(serde::Deserialize)]
struct ModelsResponse {
    data: Vec<ModelData>,
}

#[derive(serde::Deserialize)]
struct ModelData {
    id: String,
}

pub async fn configure_openai() -> Result<(), Box<dyn std::error::Error>> {
    println!("Setting up OpenAI-compatible configuration...");
    println!("This will create a config file at ~/.config/xa/config.toml");

    // Get config directory
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");

    // Create config directory if it doesn't exist
    fs::create_dir_all(&config_dir)?;

    // Get config file path
    let config_file = config_dir.join("config.toml");

    // Check if config already exists
    let config = if config_file.exists() {
        // Load existing config
        let content = fs::read_to_string(&config_file)?;
        toml::from_str(&content)?
    } else {
        // Create default config
        Config::default()
    };

    // Prompt user for configuration values
    print!("Base URL [{}]: ", config.base_url);
    io::stdout().flush()?;
    let mut base_url = String::new();
    io::stdin().read_line(&mut base_url)?;
    base_url = base_url.trim().to_string();
    if base_url.is_empty() {
        base_url = config.base_url;
    }

    print!("API Key: ");
    io::stdout().flush()?;
    let mut api_key = String::new();
    io::stdin().read_line(&mut api_key)?;
    api_key = api_key.trim().to_string();

    // Validate the API key and base URL by testing the models endpoint
    if !api_key.is_empty() {
        println!("Validating API key and base URL...");
        match fetch_models(&base_url, &api_key).await {
            Ok(models) => {
                println!("✓ API key and base URL are valid.");
                println!("Available models:");

                // Display models in a numbered list
                for (i, model) in models.iter().enumerate() {
                    println!("  {}. {}", i + 1, model);
                }

                println!("  {}. Custom model", models.len() + 1);

                print!("Select model by number (or press Enter for default '{}'): ",
                       config.default_model.as_deref().unwrap_or("gpt-4o-mini"));
                io::stdout().flush()?;
                let mut selection = String::new();
                io::stdin().read_line(&mut selection)?;
                let selection = selection.trim();

                let selected_model = if selection.is_empty() {
                    config.default_model.unwrap_or_default()
                } else if let Ok(num) = selection.parse::<usize>() {
                    if num > 0 && num <= models.len() {
                        models[num - 1].clone()
                    } else if num == models.len() + 1 {
                        print!("Enter custom model name: ");
                        io::stdout().flush()?;
                        let mut custom_model = String::new();
                        io::stdin().read_line(&mut custom_model)?;
                        custom_model.trim().to_string()
                    } else {
                        eprintln!("Invalid selection. Using default model.");
                        config.default_model.unwrap_or_default()
                    }
                } else {
                    eprintln!("Invalid selection. Using default model.");
                    config.default_model.unwrap_or_default()
                };

                // Create new config
                let new_config = Config {
                    base_url,
                    api_key,
                    default_model: if selected_model.is_empty() { None } else { Some(selected_model) },
                };

                // Serialize and write to file
                let config_content = toml::to_string(&new_config)?;
                fs::write(&config_file, config_content)?;

                println!("Configuration saved to: {:?}", config_file);
                println!("Setup complete! You can now use xa with your commands.");

                return Ok(());
            }
            Err(e) => {
                eprintln!("⚠ Warning: Could not validate API key and base URL: {}", e);
                eprintln!("Proceeding with configuration, but API may not work correctly.");
            }
        }
    }

    // If validation failed or no API key provided, ask for model directly
    print!("Default model [{}]: ", config.default_model.as_deref().unwrap_or(""));
    io::stdout().flush()?;
    let mut default_model = String::new();
    io::stdin().read_line(&mut default_model)?;
    default_model = default_model.trim().to_string();
    if default_model.is_empty() {
        default_model = config.default_model.unwrap_or_default();
    }

    // Create new config
    let new_config = Config {
        base_url,
        api_key,
        default_model: if default_model.is_empty() { None } else { Some(default_model) },
    };

    // Serialize and write to file
    let config_content = toml::to_string(&new_config)?;
    fs::write(&config_file, config_content)?;

    println!("Configuration saved to: {:?}", config_file);
    println!("Setup complete! You can now use xa with your commands.");

    Ok(())
}

async fn fetch_models(base_url: &str, api_key: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    // Adjust the URL to ensure it has the correct format
    let models_url = if base_url.ends_with("/v1") {
        format!("{}/models", base_url)
    } else if base_url.ends_with("/v1/") {
        format!("{}models", base_url)
    } else {
        format!("{}/v1/models", base_url.trim_end_matches('/'))
    };

    let response = client
        .get(&models_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(format!("API request failed: {}", error_text).into());
    }

    let models_response: ModelsResponse = response.json().await?;
    let models: Vec<String> = models_response.data.into_iter().map(|model| model.id).collect();

    Ok(models)
}

pub async fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");
    
    let config_file = config_dir.join("config.toml");
    
    if !config_file.exists() {
        return Ok(Config::default());
    }
    
    let content = fs::read_to_string(&config_file)?;
    Ok(toml::from_str(&content)?)
}
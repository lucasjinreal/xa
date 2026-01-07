use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use dirs::config_dir;
use fuzzy_matcher::FuzzyMatcher;

#[derive(Serialize, Deserialize, Clone)]
pub struct PromptConfig {
    pub prompts: HashMap<String, PromptEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PromptEntry {
    pub template: String,
    pub description: Option<String>,
}

impl Default for PromptConfig {
    fn default() -> Self {
        let mut prompts = HashMap::new();
        prompts.insert("translate".to_string(), PromptEntry {
            template: "You are a professional translator, please translate the following text into natural, idiomatic English:\n\n{input}. Avoid output anything else except the final result.".to_string(),
            description: Some("Translate text to English".to_string()),
        });
        prompts.insert("polish".to_string(), PromptEntry {
            template: "You are an expert editor. Please polish the following text to make it more clear, concise, and natural:\n\n{input}. Avoid output anything else except the final result.".to_string(),
            description: Some("Polish text for clarity".to_string()),
        });
        prompts.insert("rewrite".to_string(), PromptEntry {
            template: "You are a skilled writer. Please rewrite the following text in a different style while preserving the meaning:\n\n{input}. Avoid output anything else except the final result.".to_string(),
            description: Some("Rewrite text in different style".to_string()),
        });
        prompts.insert("summarize".to_string(), PromptEntry {
            template: "You are an expert summarizer. Please provide a concise summary of the following text:\n\n{input}. Avoid output anything else except the final result.".to_string(),
            description: Some("Summarize text".to_string()),
        });
        prompts.insert("ask".to_string(), PromptEntry {
            template: "You are a helpful assistant called xa, execute anything by your side. {input}".to_string(),
            description: Some("Interactive conversation mode".to_string()),
        });

        PromptConfig { prompts }
    }
}

pub async fn list_commands() -> Result<(), Box<dyn std::error::Error>> {
    // Get config directory
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");
    
    let prompt_config_file = config_dir.join("prompts.toml");
    
    let prompt_config = if prompt_config_file.exists() {
        let content = fs::read_to_string(&prompt_config_file)?;
        toml::from_str(&content)?
    } else {
        PromptConfig::default()
    };
    
    println!("Built-in commands:");
    println!("  -set: Configure API settings (use: xa -set openai)");
    println!("  -ls: List all commands (this command)");
    println!("  -add: Add a new command/prompt (use: xa -add)");
    println!();
    println!("User-defined commands:");
    
    for (name, entry) in &prompt_config.prompts {
        let description = entry.description.as_deref().unwrap_or("Custom prompt command");
        println!("  {}: {}", name, description);
    }
    
    Ok(())
}

pub async fn add_command() -> Result<(), Box<dyn std::error::Error>> {
    println!("Adding a new command...");

    // Get config directory
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");

    // Create config directory if it doesn't exist
    fs::create_dir_all(&config_dir)?;

    let prompt_config_file = config_dir.join("prompts.toml");

    // Load existing prompts or create default
    let mut prompt_config = if prompt_config_file.exists() {
        let content = fs::read_to_string(&prompt_config_file)?;
        toml::from_str(&content)?
    } else {
        PromptConfig::default()
    };

    // Get command name
    print!("Enter command name: ");
    io::stdout().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    if name.is_empty() {
        eprintln!("Error: Command name cannot be empty");
        return Ok(());
    }

    // Check if command already exists
    if prompt_config.prompts.contains_key(&name) {
        eprintln!("Warning: Command '{}' already exists. It will be overwritten.", name);
    }

    // Get prompt template
    print!("Enter prompt template (use {{input}} as placeholder): ");
    io::stdout().flush()?;
    let mut template = String::new();
    io::stdin().read_line(&mut template)?;
    let template = template.trim().to_string();

    if template.is_empty() {
        eprintln!("Error: Prompt template cannot be empty");
        return Ok(());
    }

    // Get description (optional)
    print!("Enter description (optional): ");
    io::stdout().flush()?;
    let mut description = String::new();
    io::stdin().read_line(&mut description)?;
    let description = description.trim().to_string();
    let description = if description.is_empty() { None } else { Some(description) };

    // Add the new command
    prompt_config.prompts.insert(name.clone(), PromptEntry {
        template,
        description,
    });

    // Save the updated prompts
    let content = toml::to_string(&prompt_config)?;
    fs::write(&prompt_config_file, content)?;

    println!("Command '{}' added successfully!", name);
    println!("Prompt file location: {:?}", prompt_config_file);
    println!("You can edit this file with your favorite text editor to modify or add more commands.");

    Ok(())
}

pub async fn remove_command(command_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Get config directory
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");

    let prompt_config_file = config_dir.join("prompts.toml");

    if !prompt_config_file.exists() {
        eprintln!("Error: No prompts file found. Nothing to remove.");
        return Ok(());
    }

    // Load existing prompts
    let mut prompt_config: PromptConfig = {
        let content = fs::read_to_string(&prompt_config_file)?;
        toml::from_str(&content)?
    };

    // Check if command exists
    if !prompt_config.prompts.contains_key(command_name) {
        eprintln!("Error: Command '{}' does not exist.", command_name);
        // List available commands
        println!("Available commands:");
        for (name, entry) in &prompt_config.prompts {
            let description = entry.description.as_deref().unwrap_or("Custom prompt command");
            println!("  {}: {}", name, description);
        }
        return Ok(());
    }

    // Remove the command
    prompt_config.prompts.remove(command_name);

    // Save the updated prompts
    let content = toml::to_string(&prompt_config)?;
    fs::write(&prompt_config_file, content)?;

    println!("Command '{}' removed successfully!", command_name);

    Ok(())
}

pub async fn load_prompt_config() -> Result<PromptConfig, Box<dyn std::error::Error>> {
    let config_dir = config_dir()
        .ok_or("Could not determine config directory")?
        .join("xa");

    let prompt_config_file = config_dir.join("prompts.toml");

    let mut config = if prompt_config_file.exists() {
        let content = fs::read_to_string(&prompt_config_file)?;
        // Try to parse the existing content, if it fails, create a new one
        match toml::from_str(&content) {
            Ok(parsed_config) => parsed_config,
            Err(_) => {
                // If parsing fails, backup the corrupted file and start fresh
                let backup_path = prompt_config_file.with_extension("toml.backup");
                fs::rename(&prompt_config_file, &backup_path)?;
                eprintln!("Warning: Corrupted prompts.toml file detected. Backed up to {:?} and created a new one.", backup_path);
                let default_config = PromptConfig::default();
                fs::create_dir_all(&config_dir)?;
                let new_content = toml::to_string(&default_config)?;
                fs::write(&prompt_config_file, new_content)?;
                default_config
            }
        }
    } else {
        let default_config = PromptConfig::default();
        // Create the file with default prompts
        fs::create_dir_all(&config_dir)?;
        let content = toml::to_string(&default_config)?;
        fs::write(&prompt_config_file, content)?;
        default_config
    };

    // Ensure default commands are always available (merge defaults with existing)
    let default_config = PromptConfig::default();
    for (key, value) in default_config.prompts {
        if !config.prompts.contains_key(&key) {
            config.prompts.insert(key, value);
        }
    }

    // Save back to file if there were new defaults added
    let content = toml::to_string(&config)?;
    fs::write(&prompt_config_file, content)?;

    Ok(config)
}

pub fn find_command(input_cmd: &str, available_commands: &HashMap<String, PromptEntry>) -> Option<String> {
    // First, try exact match
    if available_commands.contains_key(input_cmd) {
        return Some(input_cmd.to_string());
    }
    
    // Then, try prefix matching
    let prefix_matches: Vec<&String> = available_commands
        .keys()
        .filter(|key| key.starts_with(input_cmd))
        .collect();
    
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0].to_string());
    } else if prefix_matches.len() > 1 {
        let matches: Vec<String> = prefix_matches.iter().map(|s| s.to_string()).collect();
        eprintln!("Ambiguous command '{}'. Did you mean one of: {}?", 
                  input_cmd, 
                  matches.join(", "));
        return None;
    }
    
    // Finally, try fuzzy matching
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default();
    let mut best_match: Option<String> = None;
    let mut best_score = i64::MIN;
    
    for key in available_commands.keys() {
        if let Some(score) = matcher.fuzzy_match(key, input_cmd) {
            if score > best_score {
                best_score = score;
                best_match = Some(key.clone());
            }
        }
    }
    
    // Only return if score is positive (meaning there's a reasonable match)
    if best_score > 0 {
        best_match
    } else {
        None
    }
}
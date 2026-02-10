mod config;
mod prompt;
mod llm;
mod output;
mod utils;
mod store;

use clap::{Parser, ArgAction};
use config::load_config;
use prompt::{load_prompt_config, find_command, process_template_with_args};
use llm::process_with_llm;
use output::render_output;
use utils::copy_to_clipboard;
use store::{add_secret_with_tag, search_secret};

#[derive(Parser)]
#[command(name = "xa")]
#[command(about = "Execute Anything via LLM - A CLI tool for arbitrary text processing using LLMs", long_about = None)]
struct Cli {
    /// Set configuration (e.g., xa -set openai)
    #[arg(short = 's', long = "set", value_name = "CONFIG_TYPE", conflicts_with_all = &["list", "add", "rm"])]
    set: Option<String>,

    /// List all commands (xa -ls)
    #[arg(short = 'l', long = "ls", action = ArgAction::SetTrue, conflicts_with_all = &["set", "add", "rm"])]
    list: bool,

    /// Add a new command/prompt (xa -add)
    #[arg(short = 'a', long = "add", action = ArgAction::SetTrue, conflicts_with_all = &["set", "list", "rm"])]
    add: bool,

    /// Remove a command/prompt (xa -rm)
    #[arg(short = 'r', long = "rm", value_name = "COMMAND_NAME", conflicts_with_all = &["set", "list", "add"])]
    rm: Option<String>,

    /// Reset to default prompts (xa --reset-defaults)
    #[arg(long = "reset-defaults", action = ArgAction::SetTrue, conflicts_with_all = &["set", "list", "add", "rm"])]
    reset_defaults: bool,

    /// Disable streaming mode
    #[arg(long = "no-stream", action = ArgAction::SetTrue)]
    no_stream: bool,

    /// Enable debug mode to print filled prompt
    #[arg(long = "debug", action = ArgAction::SetTrue)]
    debug: bool,

    /// Command name (e.g., translate, polish)
    command: Option<String>,

    /// Input text to process
    input: Option<String>,

    /// Additional arguments for the command
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle built-in commands first
    if let Some(ref config_type) = cli.set {
        if config_type == "openai" {
            config::configure_openai().await?;
            return Ok(());
        }
    }

    if cli.list {
        prompt::list_commands().await?;
        return Ok(());
    }

    if cli.add {
        prompt::add_command().await?;
        return Ok(());
    }

    if let Some(ref command_to_remove) = cli.rm {
        prompt::remove_command(command_to_remove).await?;
        return Ok(());
    }

    if cli.reset_defaults {
        prompt::reset_default_prompts()?;
        return Ok(());
    }

    // Process command if provided
    if let Some(command) = &cli.command {
        if command == "add" {
            let secret = cli.input.as_deref().unwrap_or("");
            let note = if cli.args.is_empty() {
                String::new()
            } else {
                cli.args.join(" ")
            };

            if secret.is_empty() || note.is_empty() {
                eprintln!("Error: Usage: xa add <secret> <note>");
                std::process::exit(1);
            }

            let config = load_config().await?;
            if config.api_key.is_empty() {
                eprintln!("Error: API key not configured. Please run 'xa --set openai' first.");
                std::process::exit(1);
            }

            add_secret_with_tag(&config, secret, &note).await?;
            return Ok(());
        }

        if command == "search" {
            let mut parts = Vec::new();
            if let Some(input) = &cli.input {
                parts.push(input.clone());
            }
            if !cli.args.is_empty() {
                parts.extend(cli.args.clone());
            }

            if parts.is_empty() {
                eprintln!("Error: Usage: xa search <query>");
                std::process::exit(1);
            }

            let query = parts.join(" ");

            let config = load_config().await?;
            if config.api_key.is_empty() {
                eprintln!("Error: API key not configured. Please run 'xa --set openai' first.");
                std::process::exit(1);
            }

            search_secret(&config, &query).await?;
            return Ok(());
        }

        if command == "ask" {
            // Special handling for the 'ask' command to enable interactive mode
            if cli.no_stream {
                // If no-stream is specified, just process the input normally
                if cli.input.is_some() {
                    process_command_with_args(&cli).await?;
                } else {
                    eprintln!("Error: No input provided for command '{}'", command);
                    std::process::exit(1);
                }
            } else {
                // Start interactive conversation mode
                start_interactive_mode().await?;
            }
        } else {
            if cli.input.is_some() {
                process_command_with_args(&cli).await?;
            } else {
                eprintln!("Error: No input provided for command '{}'", command);
                std::process::exit(1);
            }
        }
    } else if cli.input.is_some() {
        eprintln!("Error: No command provided");
        std::process::exit(1);
    } else {
        // If no command or input, show help
        println!("{}", get_help_text());
    }

    Ok(())
}

async fn process_command_with_args(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let command = cli.command.as_ref().unwrap();
    let input = cli.input.as_ref().unwrap();

    // First check if config exists
    let config = load_config().await?;

    if config.api_key.is_empty() {
        eprintln!("Error: API key not configured. Please run 'xa --set openai' first.");
        std::process::exit(1);
    }

    // Get prompt configuration
    let prompt_config = load_prompt_config().await?;

    // Find the command in prompts (with fuzzy matching)
    let matched_command = find_command(command, &prompt_config.prompts);

    match matched_command {
        Some(cmd) => {
            let prompt_entry = &prompt_config.prompts[&cmd];

            // Special handling for commands that have specific argument patterns
            let (processed_input, processed_args) = if cmd == "translate" {
                // For translate command: if input looks like a language code and we have args, swap them
                // If input is 2-3 letters and first arg is longer text, assume input is target language
                if input.chars().all(|c| c.is_ascii_alphabetic()) && input.len() >= 2 && input.len() <= 3
                   && !cli.args.is_empty() {
                    // Input looks like a language code, first arg is the text to translate
                    let text_to_translate = &cli.args[0];
                    (text_to_translate.clone(), vec![input.to_string()])
                } else {
                    // Normal case: input is the text, args are additional parameters
                    (input.to_string(), cli.args.clone())
                }
            } else {
                // For other commands, use the original logic
                (input.to_string(), cli.args.clone())
            };

            // Process the template with input and arguments using the new configurable system
            let filled_prompt = process_template_with_args(
                &prompt_entry.template,
                &processed_input,
                &processed_args,
                prompt_entry.args.as_ref()
            );

            // Print the filled prompt if debug mode is enabled
            if cli.debug {
                eprintln!("[DEBUG] Debug mode is ON");
                eprintln!("[DEBUG] Filled prompt:");
                eprintln!("---");
                eprintln!("{}", filled_prompt);
                eprintln!("---");
                eprintln!("[DEBUG] End of filled prompt\n");
            }

            // Call the LLM API with streaming option
            let result = process_with_llm(&config, &filled_prompt, !cli.no_stream).await?;

            // Copy result to clipboard
            if let Err(e) = copy_to_clipboard(&result) {
                eprintln!("Warning: Could not copy to clipboard: {}", e);
            }

            // Render the result with Markdown support
            render_output(&result, true); // true for success message

            Ok(())
        }
        None => {
            eprintln!("Error: Command '{}' not found. Use 'xa -ls' to see available commands.", command);
            std::process::exit(1);
        }
    }
}

async fn process_command(command: String, input: String, stream: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Create a temporary CLI struct to pass to the new function
    let temp_cli = Cli {
        set: None,
        list: false,
        add: false,
        rm: None,
        reset_defaults: false,
        no_stream: !stream,
        debug: false,
        command: Some(command),
        input: Some(input),
        args: vec![],
    };

    process_command_with_args(&temp_cli).await
}

use std::io::{self, Write};
use termimad::{MadSkin, ansi};

async fn start_interactive_mode() -> Result<(), Box<dyn std::error::Error>> {
    // First check if config exists
    let config = load_config().await?;

    if config.api_key.is_empty() {
        eprintln!("Error: API key not configured. Please run 'xa --set openai' first.");
        std::process::exit(1);
    }

    // Create a colorful skin for the interactive mode
    let mut skin = MadSkin::default();
    skin.set_headers_fg(ansi(35)); // Magenta for headers
    skin.bold.set_fg(ansi(33)); // Yellow for bold text

    // Print welcome message with colors
    skin.print_text("## Welcome to xa Interactive Mode\n\n");
    println!("{}", "\x1b[90mType your message and press Enter. Type 'exit', 'quit', or 'bye' to end, or press Ctrl+C to exit.\x1b[0m");
    println!("{}", "\x1b[90mUse 'clear' to clear conversation history, 'history' to view recent exchanges.\x1b[0m");
    println!();

    // Initialize conversation history
    let mut conversation_history = Vec::new();

    loop {
        // Print colorful prompt
        print!("\x1b[36m>\x1b[0m "); // Cyan prompt
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // Check if input is empty (user pressed Enter without typing)
        if input.is_empty() {
            continue;
        }

        // Check for special commands
        match input.to_lowercase().as_str() {
            "exit" | "quit" | "bye" => {
                println!("{}", "\x1b[90mGoodbye! Thanks for using xa.\x1b[0m");
                break;
            }
            "clear" => {
                conversation_history.clear();
                println!("{}", "\x1b[90mConversation history cleared.\x1b[0m");
                continue;
            }
            "history" => {
                if conversation_history.is_empty() {
                    println!("{}", "\x1b[90mNo conversation history yet.\x1b[0m");
                } else {
                    println!("{}", "\x1b[90mRecent conversation history:\x1b[0m");
                    for (i, (user_msg, ai_resp)) in conversation_history.iter().enumerate() {
                        println!("\x1b[90m[{}]\x1b[0m \x1b[33mYou:\x1b[0m {}", i + 1, user_msg);
                        println!("\x1b[90m    \x1b[32mAI:\x1b[0m {}", ai_resp);
                        println!();
                    }
                }
                continue;
            }
            _ => {}
        }

        // Add user message to conversation history
        conversation_history.push((input.to_string(), String::new()));

        // Build the full prompt with conversation history
        let mut full_prompt = String::new();
        full_prompt.push_str("You are a helpful assistant called xa, execute anything by your side.\n\n");

        if !conversation_history.is_empty() {
            full_prompt.push_str("Previous conversation:\n");
            for (user_msg, ai_resp) in &conversation_history[..conversation_history.len()-1] {
                full_prompt.push_str(&format!("User: {}\n", user_msg));
                if !ai_resp.is_empty() {
                    full_prompt.push_str(&format!("Assistant: {}\n", ai_resp));
                }
            }
            full_prompt.push_str("\n");
        }

        full_prompt.push_str(&format!("Current message: {}", input));

        // Call the LLM API with streaming
        let result = process_with_llm(&config, &full_prompt, true).await?;

        // Copy result to clipboard
        if let Err(e) = copy_to_clipboard(&result) {
            eprintln!("Warning: Could not copy to clipboard: {}", e);
        }

        // Update the conversation history with the AI response
        if let Some(last) = conversation_history.last_mut() {
            last.1 = result.clone();
        }

        // In interactive mode, the content is already streamed to the terminal,
        // so we don't need to render it again with render_output.
        // Just add a separator for readability
        println!(); // Add a blank line for readability
    }

    Ok(())
}

fn get_help_text() -> String {
    r#"xa - Execute Anything via LLM

USAGE:
    xa [OPTIONS] [COMMAND] [INPUT]

OPTIONS:
    -s, --set <CONFIG_TYPE>     Configure API settings (e.g., xa --set openai)
    -l, --ls                    List all available commands
    -a, --add                   Add a new command/prompt
    -r, --rm <COMMAND_NAME>     Remove a command/prompt
    --reset-defaults            Reset to default prompts
    --no-stream                 Disable streaming mode
    --debug                     Enable debug mode to print filled prompt

EXAMPLES:
    xa --set openai              # Configure OpenAI-compatible API
    xa --ls                      # List all commands
    xa --add                     # Add a new command
    xa --rm summarize            # Remove the 'summarize' command
    xa --reset-defaults          # Reset to default prompts
    xa add mysecret "this is a gitcode apikey"  # Add a secret with auto tag
    xa search "my gitcode token" # Search secrets
    xa translate "Hello"        # Translate text
    xa trans "Hello"            # Translate using fuzzy matching
    xa polish "This is a draft text" --no-stream  # Polish text without streaming
    xa --debug trans zh "Hello"  # Translate with debug mode enabled (debug flag before command)

For more information, visit the project repository."#.to_string()
}

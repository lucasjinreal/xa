mod config;
mod prompt;
mod llm;
mod output;
mod utils;

use clap::{Parser, ArgAction};
use config::load_config;
use prompt::{load_prompt_config, find_command};
use llm::process_with_llm;
use output::render_output;
use utils::copy_to_clipboard;

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

    /// Disable streaming mode
    #[arg(long = "no-stream", action = ArgAction::SetTrue)]
    no_stream: bool,

    /// Command name (e.g., translate, polish)
    command: Option<String>,

    /// Input text to process
    input: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle built-in commands first
    if let Some(config_type) = cli.set {
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

    if let Some(command_to_remove) = cli.rm {
        prompt::remove_command(&command_to_remove).await?;
        return Ok(());
    }

    // Process command if provided
    if let Some(command) = cli.command {
        if command == "ask" {
            // Special handling for the 'ask' command to enable interactive mode
            if cli.no_stream {
                // If no-stream is specified, just process the input normally
                if let Some(input) = cli.input {
                    process_command(command, input, false).await?;
                } else {
                    eprintln!("Error: No input provided for command '{}'", command);
                    std::process::exit(1);
                }
            } else {
                // Start interactive conversation mode
                start_interactive_mode().await?;
            }
        } else {
            if let Some(input) = cli.input {
                process_command(command, input, !cli.no_stream).await?;
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

async fn process_command(command: String, input: String, stream: bool) -> Result<(), Box<dyn std::error::Error>> {
    // First check if config exists
    let config = load_config().await?;

    if config.api_key.is_empty() {
        eprintln!("Error: API key not configured. Please run 'xa -set openai' first.");
        std::process::exit(1);
    }

    // Get prompt configuration
    let prompt_config = load_prompt_config().await?;

    // Find the command in prompts (with fuzzy matching)
    let matched_command = find_command(&command, &prompt_config.prompts);

    match matched_command {
        Some(cmd) => {
            let prompt_entry = &prompt_config.prompts[&cmd];
            let filled_prompt = prompt_entry.template.replace("{input}", &input);

            // Call the LLM API with streaming option
            let result = process_with_llm(&config, &filled_prompt, stream).await?;

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

use std::io::{self, Write};

async fn start_interactive_mode() -> Result<(), Box<dyn std::error::Error>> {
    // First check if config exists
    let config = load_config().await?;

    if config.api_key.is_empty() {
        eprintln!("Error: API key not configured. Please run 'xa -set openai' first.");
        std::process::exit(1);
    }

    println!("Starting interactive mode. Type your message and press Enter. Type 'exit' or 'quit' to end, or press Ctrl+C to exit.");
    println!();

    loop {
        // Print prompt and read user input
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // Check if input is empty (user pressed Enter without typing)
        if input.is_empty() {
            continue;
        }

        // Check for exit command
        if input.to_lowercase() == "exit" || input.to_lowercase() == "quit" {
            println!("Goodbye!");
            break;
        }

        // Process the input with the ask prompt
        let filled_prompt = format!("You are a helpful assistant called xa, execute anything by your side. {}", input);

        // Call the LLM API with streaming
        let result = process_with_llm(&config, &filled_prompt, true).await?;

        // Copy result to clipboard
        if let Err(e) = copy_to_clipboard(&result) {
            eprintln!("Warning: Could not copy to clipboard: {}", e);
        }

        // Render the result with Markdown support
        render_output(&result, false); // false to not show success message since we're in a loop

        println!(); // Add a blank line for readability
    }

    Ok(())
}

fn get_help_text() -> String {
    r#"xa - Execute Anything via LLM

USAGE:
    xa [OPTIONS] [COMMAND] [INPUT]

OPTIONS:
    -s, --set <CONFIG_TYPE>     Configure API settings (e.g., xa -set openai)
    -l, --ls                    List all available commands
    -a, --add                   Add a new command/prompt
    -r, --rm <COMMAND_NAME>     Remove a command/prompt
    --no-stream                 Disable streaming mode

EXAMPLES:
    xa --set openai              # Configure OpenAI-compatible API
    xa --ls                      # List all commands
    xa --add                     # Add a new command
    xa --rm summarize            # Remove the 'summarize' command
    xa translate "Hello"        # Translate text
    xa trans "Hello"            # Translate using fuzzy matching
    xa polish "This is a draft text" --no-stream  # Polish text without streaming

For more information, visit the project repository."#.to_string()
}
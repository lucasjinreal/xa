mod config;
mod prompt;
mod llm;
mod output;
mod utils;
mod store;
mod agent;
mod output_filter;
mod session;
mod tools;
mod tui;

use clap::{Parser, Subcommand};
use config::load_config;
use prompt::{load_prompt_config, find_command, process_template_with_args};
use llm::process_with_llm;
use output::render_output;
use utils::copy_to_clipboard;
use store::{add_secret_with_tag, search_secret};
use session::Session;

#[derive(Parser)]
#[command(name = "xa")]
#[command(about = "xa - a lightweight coding-agent CLI (like codex / claude-code)")]
#[command(after_help = "Launch the agent with `xa` or `xa chat`. Configure a provider with `xa login`.\nInside the TUI use:\n  /login [name]  - set a provider (custom endpoint + key + model)\n  /models [name] - switch provider or set the model\n  /save [title]  - save the conversation as a session\n  /sessions      - list saved sessions\nResume a session: xa resume [id]")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Disable streaming mode
    #[arg(long = "no-stream", global = true)]
    no_stream: bool,

    /// Enable debug mode to print filled prompt
    #[arg(long = "debug", global = true)]
    debug: bool,

    /// Input text to process
    input: Option<String>,

    /// Additional arguments for the command
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Set configuration (e.g., xa set openai)
    #[command(short_flag = 's')]
    Set {
        /// Configuration type
        config_type: String,
    },

    /// List all commands or specific items
    #[command(short_flag = 'l', alias = "list")]
    Ls {
        /// Type of items to list (prompts, stores)
        #[arg(value_name = "TYPE")]
        list_type: Option<String>,
    },

    /// Add a new command/prompt
    #[command(short_flag = 'a')]
    Add,

    /// Remove a command/prompt
    #[command(short_flag = 'r')]
    Rm {
        /// Command name to remove
        command_name: String,
    },

    /// Reset to default prompts
    #[command(alias = "reset")]
    ResetDefaults,

    /// Add a secret with auto tag
    #[command(short_flag = 'A', visible_alias = "as")]
    AddSecret {
        /// The secret value
        secret: String,
        /// Note/description for the secret
        note: String,
    },

    /// Search secrets by natural language
    #[command(visible_alias = "se")]
    Search {
        /// Search query
        query: String,
    },

    /// Interactive conversation mode
    Ask,

    /// Launch the codex-like interactive coding TUI
    Chat,

    /// Configure a provider (custom endpoint + key + model) and save it
    Login {
        /// Provider name (prompted if omitted)
        name: Option<String>,
    },

    /// Resume a saved session, or choose one interactively
    Resume {
        /// Session id (opens the picker when omitted)
        id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle commands via subcommand matching
    match cli.command {
        Some(Commands::Set { config_type }) => {
            if config_type == "openai" {
                config::configure_openai().await?;
                return Ok(());
            } else {
                eprintln!("Unknown configuration type: {}", config_type);
                eprintln!("Available configuration types: openai");
                std::process::exit(1);
            }
        }
        Some(Commands::Ls { list_type }) => {
            match list_type.as_deref() {
                Some("prompts") => {
                    prompt::list_prompts().await?;
                    return Ok(());
                }
                Some("stores") => {
                    store::list_stores().await?;
                    return Ok(());
                }
                Some(other) => {
                    eprintln!("Unknown list type: {}", other);
                    eprintln!("Available list types: prompts, stores");
                    eprintln!("Usage: xa ls prompts  or  xa ls stores");
                    std::process::exit(1);
                }
                None => {
                    prompt::list_commands().await?;
                    return Ok(());
                }
            }
        }
        Some(Commands::Add) => {
            prompt::add_command().await?;
            return Ok(());
        }
        Some(Commands::Rm { command_name }) => {
            prompt::remove_command(&command_name).await?;
            return Ok(());
        }
        Some(Commands::ResetDefaults) => {
            prompt::reset_default_prompts()?;
            return Ok(());
        }
        Some(Commands::AddSecret { secret, note }) => {
            let config = load_config().await?;
            if config.api_key.is_empty() {
                eprintln!("Error: API key not configured. Please run 'xa set openai' first.");
                std::process::exit(1);
            }
            add_secret_with_tag(&config, &secret, &note).await?;
            return Ok(());
        }
        Some(Commands::Search { query }) => {
            let config = load_config().await?;
            if config.api_key.is_empty() {
                eprintln!("Error: API key not configured. Please run 'xa set openai' first.");
                std::process::exit(1);
            }
            search_secret(&config, &query).await?;
            return Ok(());
        }
        Some(Commands::Ask) => {
            if cli.input.is_some() {
                // Process with ask command if input provided
                process_command_with_args(&cli, "ask").await?;
            } else {
                // Start interactive conversation mode
                start_interactive_mode().await?;
            }
            return Ok(());
        }
        Some(Commands::Chat) => {
            let provider = agent::load_active_provider().await;
            let session = Session::new(&provider.name, &provider.model);
            tui::run(provider, session).await?;
            return Ok(());
        }
        Some(Commands::Login { name }) => {
            run_login(name).await?;
            return Ok(());
        }
        Some(Commands::Resume { id }) => {
            resume_session(id).await?;
            return Ok(());
        }
        None => {
            // No subcommand provided -> launch the interactive agent TUI directly.
            if cli.input.is_some() {
                eprintln!("Error: No command provided");
                std::process::exit(1);
            } else {
                let provider = agent::load_active_provider().await;
                let session = Session::new(&provider.name, &provider.model);
                tui::run(provider, session).await?;
            }
            return Ok(());
        }
    }
}

async fn process_command_with_args(cli: &Cli, command_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let input = cli.input.as_ref().unwrap();

    // First check if config exists
    let config = load_config().await?;

    if config.api_key.is_empty() {
        eprintln!("Error: API key not configured. Please run 'xa set openai' first.");
        std::process::exit(1);
    }

    // Get prompt configuration
    let prompt_config = load_prompt_config().await?;

    // Find the command in prompts (with fuzzy matching)
    let matched_command = find_command(command_name, &prompt_config.prompts);

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
            eprintln!("Error: Command '{}' not found. Use 'xa ls' to see available commands.", command_name);
            std::process::exit(1);
        }
    }
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
    r#"xa - a lightweight coding-agent CLI (like codex / claude-code)

USAGE:
    xa [OPTIONS]                 Launch the interactive agent TUI
    xa chat                     Launch the interactive agent TUI
    xa <SUBCOMMAND> ...         Legacy prompt commands (translate, polish, ...)

COMMANDS (agent):
    chat                        Start the interactive coding-agent TUI
    login [name]                Configure a provider (endpoint + key + model)
    resume [id]                 Resume a session, or open the session picker

IN-TUI SLASH COMMANDS:
    /login [name]               Set a provider (custom endpoint + key + model)
    /models [name|model]        Switch active provider, or set the model
    /sessions                   List saved sessions
    /save [title]              Save the current conversation as a session
    /new                        Start a fresh session
    /clear /help /exit          Clear / help / quit

OPTIONS:
    --no-stream                 Disable streaming (legacy prompt mode)
    --debug                     Print the filled prompt (legacy prompt mode)
    -h, --help                 Print help

EXAMPLES:
    xa                                  # launch the agent TUI
    xa chat                            # launch the agent TUI
    xa resume                         # choose a saved session
    xa resume ab12                    # resume a saved session directly
    /login mygateway                  # inside TUI: point at any OpenAI-compatible endpoint
    /models gpt-4o                   # inside TUI: switch model

For more information, visit the project repository."#.to_string()
}

/// `xa login [name]` — launch the codex-style interactive provider setup
/// (select provider → optional API key → auto-query models → pick a model),
/// rendered in its own alternate-screen terminal. The chosen provider is
/// persisted as the active one.
async fn run_login(name: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    match tui::wizard::Wizard::run_standalone(tui::wizard::WizardMode::Login, name.as_deref()).await? {
        Some(p) => println!("logged in as provider `{}` (model `{}`)", p.name, p.model),
        None => println!("login cancelled"),
    }
    Ok(())
}

/// Resume a named session, or open the session picker when no id was given.
async fn resume_session(id: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let id = match id {
        Some(id) => id,
        None => match tui::resume::pick_session()? {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let session = session::load(&id).ok_or_else(|| format!("Session not found: {id}"))?;
    let provider = agent::load_active_provider().await;
    tui::run(provider, session).await?;
    Ok(())
}

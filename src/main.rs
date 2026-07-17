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
use chrono::{Local, TimeZone};
use std::collections::BTreeMap;
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
#[command(after_help = "Launch the agent with `xa` or `xa chat`. Configure a provider with `xa login`.\nInside the TUI use:\n  /login [name]  - set a provider (custom endpoint + key + model)\n  /models [name] - switch provider or set the model\n  /save [title]  - save the conversation as a session\n  /sessions      - list saved sessions\nResume a session: xa resume [id]\nReview saved tool-output gains: xa gain [--daily|--weekly|--monthly|--all]")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Disable streaming mode
    #[arg(long = "no-stream", global = true)]
    no_stream: bool,

    /// Enable debug mode to print filled prompt
    #[arg(long = "debug", global = true)]
    debug: bool,

    /// TUI color theme: auto (detect terminal), dark, or light
    #[arg(
        long = "theme",
        global = true,
        value_name = "MODE",
        value_parser = ["auto", "dark", "light"]
    )]
    theme: Option<String>,

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

    /// Review saved tool-output and API token usage across sessions
    Gain {
        /// Break down totals by calendar day
        #[arg(long)]
        daily: bool,
        /// Break down totals by ISO week
        #[arg(long)]
        weekly: bool,
        /// Break down totals by calendar month
        #[arg(long)]
        monthly: bool,
        /// Include all recorded sessions (the default is the all-time total)
        #[arg(long)]
        all: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Install TUI palette early (chat / resume / login all share it).
    init_tui_theme(&cli);

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
        Some(Commands::Gain { daily, weekly, monthly, all }) => {
            print_gain(daily, weekly, monthly, all)?;
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
    gain [--daily|--weekly|--monthly|--all]
                                Review saved token usage and tool-output gains

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
    --theme <MODE>              TUI theme: auto (default), dark, or light
    -h, --help                 Print help

EXAMPLES:
    xa                                  # launch the agent TUI
    xa chat                            # launch the agent TUI
    xa --theme light                  # force light terminal palette
    xa resume                         # choose a saved session
    xa resume ab12                    # resume a saved session directly
    xa gain                           # all-time token and tool-output gain
    xa gain --weekly                  # weekly gain breakdown
    /login mygateway                  # inside TUI: point at any OpenAI-compatible endpoint
    /models gpt-4o                   # inside TUI: switch model

Theme (TUI): CLI --theme > env XA_THEME > config.toml theme = \"auto|dark|light\" > auto-detect.

For more information, visit the project repository."#.to_string()
}

/// Resolve theme preference: CLI `--theme` > `XA_THEME` > config.toml > auto.
fn resolve_theme_preference(cli: &Cli) -> tui::ThemePreference {
    if let Some(ref s) = cli.theme {
        if let Some(pref) = tui::ThemePreference::parse(s) {
            return pref;
        }
        eprintln!(
            "Warning: unknown --theme '{s}', expected auto|dark|light; using auto"
        );
    }
    if let Ok(s) = std::env::var("XA_THEME") {
        if let Some(pref) = tui::ThemePreference::parse(&s) {
            return pref;
        }
    }
    if let Some(s) = config::load_theme_setting() {
        if let Some(pref) = tui::ThemePreference::parse(&s) {
            return pref;
        }
    }
    tui::ThemePreference::Auto
}

fn init_tui_theme(cli: &Cli) {
    let pref = resolve_theme_preference(cli);
    let mode = tui::init_from_preference(pref);
    if cli.debug {
        eprintln!(
            "[DEBUG] theme preference={} resolved={}",
            pref.as_str(),
            match mode {
                tui::ColorMode::Dark => "dark",
                tui::ColorMode::Light => "light",
            }
        );
    }
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

#[derive(Clone, Copy, PartialEq)]
enum GainPeriod {
    Overall,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Default)]
struct GainTotals {
    sessions: usize,
    tool_calls: usize,
    raw_bytes: usize,
    returned_bytes: usize,
    estimated_saved_tokens: usize,
    api_requests: u64,
    api_prompt_tokens: u64,
    api_completion_tokens: u64,
    api_total_tokens: u64,
}

impl GainTotals {
    fn add_record(&mut self, record: &session::GainSessionRecord) {
        self.sessions += 1;
        self.api_requests += record.api_token_usage.requests;
        self.api_prompt_tokens += record.api_token_usage.prompt_tokens;
        self.api_completion_tokens += record.api_token_usage.completion_tokens;
        self.api_total_tokens += record.api_token_usage.total_tokens;
        for call in &record.output_filter_calls {
            self.tool_calls += 1;
            self.raw_bytes += call.raw_bytes;
            self.returned_bytes += call.returned_bytes;
            self.estimated_saved_tokens += call.estimated_tokens_saved;
        }
    }

    fn add_call(&mut self, call: &crate::output_filter::ToolOutputStats) {
        self.tool_calls += 1;
        self.raw_bytes += call.raw_bytes;
        self.returned_bytes += call.returned_bytes;
        self.estimated_saved_tokens += call.estimated_tokens_saved;
    }

    fn bytes_saved(&self) -> usize {
        self.raw_bytes.saturating_sub(self.returned_bytes)
    }

    fn savings_percent(&self) -> f64 {
        if self.raw_bytes == 0 { 0.0 } else { self.bytes_saved() as f64 * 100.0 / self.raw_bytes as f64 }
    }
}

/// Print the saved session-level accounting. This deliberately reads only the
/// serialized aggregate fields, never a conversation's message content.
fn print_gain(daily: bool, weekly: bool, monthly: bool, _all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let requested = [daily, weekly, monthly].into_iter().filter(|enabled| *enabled).count();
    if requested > 1 {
        return Err("choose only one of --daily, --weekly, or --monthly".into());
    }
    let period = if daily { GainPeriod::Daily } else if weekly { GainPeriod::Weekly } else if monthly { GainPeriod::Monthly } else { GainPeriod::Overall };
    let records = session::gain_records();
    if records.is_empty() {
        println!("No saved session usage yet.");
        return Ok(());
    }

    let mut overall = GainTotals::default();
    let mut periods: BTreeMap<String, GainTotals> = BTreeMap::new();
    let mut commands: BTreeMap<String, GainTotals> = BTreeMap::new();
    for record in &records {
        overall.add_record(record);
        let key = gain_period_label(record.updated, period);
        let bucket = periods.entry(key).or_default();
        bucket.add_record(record);
        for call in &record.output_filter_calls {
            let label = if call.command.is_empty() {
                format!("{}/{}", call.tool, call.filter)
            } else {
                call.command.clone()
            };
            commands.entry(label).or_default().add_call(call);
        }
    }

    println!("\nTool output gain");
    print_gain_totals("All time", &overall);
    if period != GainPeriod::Overall {
        println!("\nPeriod                 Calls    Saved bytes   Saved    Est. tokens");
        for (label, totals) in periods {
            println!(
                "{label:<22} {:>5} {:>14} {:>6.1}% {:>14}",
                totals.tool_calls,
                format_count(totals.bytes_saved()),
                totals.savings_percent(),
                format!("~{}", format_count(totals.estimated_saved_tokens)),
            );
        }
    }
    if !commands.is_empty() {
        let mut commands: Vec<_> = commands.into_iter().collect();
        commands.sort_by(|(left_label, left), (right_label, right)| {
            right.bytes_saved().cmp(&left.bytes_saved()).then_with(|| left_label.cmp(right_label))
        });
        println!("\nBy command              Calls    Saved bytes   Saved    Est. tokens");
        for (command, totals) in commands.into_iter().take(12) {
            println!(
                "{:<22} {:>5} {:>14} {:>6.1}% {:>14}",
                truncate_label(&command, 22),
                totals.tool_calls,
                format_count(totals.bytes_saved()),
                totals.savings_percent(),
                format!("~{}", format_count(totals.estimated_saved_tokens)),
            );
        }
    }
    Ok(())
}

fn print_gain_totals(label: &str, totals: &GainTotals) {
    println!("{label}");
    println!("  Sessions:              {}", format_count(totals.sessions));
    println!("  Tool output:           {} → {} bytes", format_count(totals.raw_bytes), format_count(totals.returned_bytes));
    println!("  Tool output saved:     {} bytes ({:.1}%) · ~{} tokens across {} calls", format_count(totals.bytes_saved()), totals.savings_percent(), format_count(totals.estimated_saved_tokens), format_count(totals.tool_calls));
    println!("  API token usage:       {} total ({} prompt · {} completion across {} requests)", format_count(totals.api_total_tokens as usize), format_count(totals.api_prompt_tokens as usize), format_count(totals.api_completion_tokens as usize), format_count(totals.api_requests as usize));
}

fn gain_period_label(timestamp_ms: i64, period: GainPeriod) -> String {
    let time = Local.timestamp_millis_opt(timestamp_ms).single().unwrap_or_else(Local::now);
    match period {
        GainPeriod::Daily => time.format("%Y-%m-%d").to_string(),
        GainPeriod::Weekly => time.format("%G-W%V").to_string(),
        GainPeriod::Monthly => time.format("%Y-%m").to_string(),
        GainPeriod::Overall => "all time".to_string(),
    }
}

fn format_count(value: usize) -> String {
    let text = value.to_string();
    let mut formatted = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().enumerate() {
        if index > 0 && (text.len() - index) % 3 == 0 { formatted.push(','); }
        formatted.push(ch);
    }
    formatted
}

fn truncate_label(label: &str, width: usize) -> String {
    if label.chars().count() <= width { return label.to_string(); }
    let mut output: String = label.chars().take(width.saturating_sub(1)).collect();
    output.push('…');
    output
}

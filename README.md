# xa - Execute Anything via LLM

xa is a minimal yet powerful CLI executor that enables arbitrary text processing through user-defined prompts + LLMs, such as translation, polishing, rewriting, continuation, summarization, etc.

## Core Philosophy

User defines intent. xa executes it.

Compared to opening ChatGPT or web tools, xa aims to be:

- Faster
- Composable
- Scriptable
- Available anywhere (Terminal-first)

## Installation

```bash
# Clone the repository
git clone <repository-url>
cd xa

# Build the project
cargo build --release

# The binary will be available at target/release/xa
```

## Usage

### Configuration

First, configure your LLM API settings:

```bash
xa --set openai
```

This will prompt you for:
- Base URL (default: https://api.openai.com/v1)
- API Key
- Default model (default: gpt-4o-mini)

Configuration is stored in `~/.config/xa/config.toml`.

### Available Commands

List all available commands:

```bash
xa --ls
```

### Adding Custom Commands

Add new commands with custom prompts:

```bash
xa --add
```

This will prompt you to enter:
- Command name
- Prompt template (use `{input}` as placeholder)
- Optional description

The prompts are stored in `~/.config/xa/prompts.toml` and can be edited with your favorite text editor.

### Removing Commands

Remove existing commands:

```bash
xa --rm command_name
```

### Configuration

Configure your LLM API settings:

```bash
xa --set openai
```

During setup, xa will:
- Validate your API key and base URL
- List available models to choose from
- Allow custom model selection

### Running Commands

Execute a command with input:

```bash
xa translate "Hello, how are you?"
xa polish "This is a draft text that needs improvement"
xa summarize "Long text to summarize..."
```

### Streaming Mode

By default, xa streams the response from the LLM in real-time. To disable streaming:

```bash
xa translate "Hello" --no-stream
```

### Fuzzy Command Matching

xa supports fuzzy command matching:

```bash
xa trans "Hello"  # Matches to 'translate'
xa pol "text"     # Matches to 'polish'
```

## Features

- **LLM Integration**: Supports OpenAI-compatible APIs
- **Prompt Management**: Define custom prompts for different tasks
- **Streaming Support**: Real-time response streaming by default
- **Fuzzy Matching**: Command abbreviations and fuzzy matching
- **Markdown Rendering**: Rich output with Markdown support
- **Clipboard Integration**: Results automatically copied to clipboard
- **Performance Metrics**: Shows execution time

## Built-in Commands

- `-s, --set`: Configure API settings
- `-l, --ls`: List all available commands
- `-a, --add`: Add a new command/prompt
- `-r, --rm`: Remove a command/prompt
- `translate`: Translate text
- `polish`: Polish text for clarity
- `rewrite`: Rewrite text in different style
- `summarize`: Summarize text

## Configuration

- API configuration: `~/.config/xa/config.toml`
- Custom prompts: `~/.config/xa/prompts.toml`

## License

MIT
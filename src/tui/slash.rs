//! Slash command popup (DESIGN.md §5).
//!
//! The `/`-prefixed command menu is a floating overlay driven by a subsequence
//! fuzzy filter over the static command list.

pub struct SlashCommand {
    pub name: &'static str,
    pub desc: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "/login", desc: "add or update a provider" },
    SlashCommand { name: "/models", desc: "switch provider / set model" },
    SlashCommand { name: "/clear", desc: "clear the conversation" },
    SlashCommand { name: "/help", desc: "show help" },
    SlashCommand { name: "/exit", desc: "quit" },
    SlashCommand { name: "/sessions", desc: "list saved sessions" },
    SlashCommand { name: "/tools", desc: "list available tools" },
    SlashCommand { name: "/save", desc: "save the current session" },
    SlashCommand { name: "/new", desc: "start a new session" },
];

/// Subsequence fuzzy match: every char of `query` appears in order in `text`.
pub fn fuzzy_subseq(query: &str, text: &str) -> bool {
    let mut it = text.chars();
    for q in query.chars() {
        let q = q.to_ascii_lowercase();
        loop {
            match it.next() {
                Some(c) if c.to_ascii_lowercase() == q => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

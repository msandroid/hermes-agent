use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use reqwest::Url;

use hermes_agent_rs::{Completions, Conversation, NoTools, Tools};
mod gateway_proxy;
mod history;
mod sqlite_history;
mod todo;

#[derive(Parser)]
#[command(version, about = "Hermes Rust chat preview")]
struct Args {
    #[arg(short, long)]
    query: Option<String>,
    #[arg(long, value_name = "PATH", conflicts_with = "query")]
    query_file: Option<PathBuf>,
    #[arg(short, long)]
    model: String,
    #[arg(long)]
    base_url: Url,
    /// Save completed turns to a Rust transcript and resume it on the next run.
    #[arg(long)]
    history: Option<PathBuf>,
    /// Read a completed conversation from the existing Python state database.
    #[arg(long, requires = "resume_session")]
    state_db: Option<PathBuf>,
    /// Exact session ID in the existing Python state database.
    #[arg(long, requires = "state_db", conflicts_with = "history")]
    resume_session: Option<String>,
    /// Serve the existing Hermes gateway proxy protocol on a loopback address.
    #[arg(long, conflicts_with_all = ["query", "query_file", "history", "state_db", "resume_session"])]
    gateway_listen: Option<std::net::SocketAddr>,
    /// Permit the model to read and update the conversation-local todo list.
    #[arg(long, conflicts_with = "gateway_listen")]
    allow_todo: bool,
}

impl Args {
    fn prompt(&self) -> Result<Option<String>, String> {
        let Some(path) = &self.query_file else {
            return Ok(self.query.clone());
        };
        let bytes = if path.as_os_str() == "-" {
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes).map(|_| bytes)
        } else {
            fs::read(path)
        }
        .map_err(|err| format!("cannot read query file {}: {err}", path.display()))?;
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        Ok(Some(
            String::from_utf8_lossy(bytes)
                .replace("\r\n", "\n")
                .replace('\r', "\n"),
        ))
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let prompt = match args.prompt() {
        Ok(Some(prompt)) if prompt.trim().is_empty() => {
            eprintln!("Error: query is empty");
            return ExitCode::from(2);
        }
        Ok(prompt) => prompt,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::from(2);
        }
    };

    match run(args, prompt) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args, prompt: Option<String>) -> Result<(), String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.is_empty());
    let client = Completions::new(args.base_url, api_key, args.model)?;
    if let Some(address) = args.gateway_listen {
        return gateway_proxy::serve(address, client);
    }
    let (history, messages) = match args.history {
        Some(path) => {
            let (history, messages) = history::History::open(path)?;
            (Some(history), messages)
        }
        None => {
            let messages = match (args.state_db, args.resume_session) {
                (Some(path), Some(id)) => sqlite_history::load(&path, &id)?,
                _ => Vec::new(),
            };
            (None, messages)
        }
    };
    if args.allow_todo {
        let tool = todo::Todo::restore(&messages);
        return run_conversation(Conversation::new(messages, tool), client, history, prompt);
    }
    run_conversation(
        Conversation::new(messages, NoTools),
        client,
        history,
        prompt,
    )
}

fn run_conversation(
    mut conversation: Conversation<impl Tools>,
    client: Completions,
    history: Option<history::History>,
    prompt: Option<String>,
) -> Result<(), String> {
    let mut turn = |prompt: &str| {
        let answer = conversation.turn(&client, prompt)?;
        if let Some(history) = &history {
            history.save(conversation.messages())?;
        }
        println!("{answer}");
        Ok(())
    };
    if let Some(prompt) = prompt {
        return turn(&prompt);
    }
    repl(turn)
}

fn repl(mut turn: impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    let mut lines = stdin.lock().lines();
    loop {
        if interactive {
            eprint!("hermes> ");
            io::stderr().flush().map_err(|err| err.to_string())?;
        }
        let Some(line) = lines.next().transpose().map_err(|err| err.to_string())? else {
            return Ok(());
        };
        match line.trim() {
            "/exit" | "/quit" => return Ok(()),
            "" => continue,
            _ => turn(&line)?,
        }
    }
}

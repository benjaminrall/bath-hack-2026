//! batstone - a coding agent that evolves itself
//!
//! Commands:
//!   /quit, /exit    Exit the agent
//!   /clear          Clear conversation history
//!   /model <name>   Switch model mid-session

use rig::agent::Agent;
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::{CompletionModel, Prompt, ToolDefinition, Usage};
use rig::providers::openrouter;
use rig::tool::{Tool, ToolError};
use rig::wasm_compat::{WasmCompatSend, WasmCompatSync};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::to_value;
use std::io;
use std::io::{BufRead, IsTerminal, Read, Write};

// ANSI colour helpers
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";

const SYSTEM_PROMPT: &str = r#"You are a coding assistant working in the user's terminal.
You have access to the filesystem and shell. Be direct and concise.
When the user asks you to do something, do it — don't just explain how.
Use tools proactively: read files to understand context, run commands to verify your work.
After making changes, run tests or verify the result when appropriate."#;

fn print_banner() {
    println!("\n{BOLD}{CYAN}  batstone{RESET} {DIM}— our evolving coding agent{RESET}");
    println!("{DIM}  Type /quit to exit, /clear to reset{RESET}\n");
}

fn print_usage(usage: &Usage) {
    if usage.input_tokens > 0 || usage.output_tokens > 0 {
        println!(
            "\n{DIM}  tokens: {} in / {} out{RESET}",
            usage.input_tokens, usage.output_tokens
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // Creates OpenRouter client
    let client = openrouter::Client::from_env();

    // Read environment arguments
    let args: Vec<String> = std::env::args().collect();

    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "openai/gpt-4o".into());

    let skill_dirs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--skills")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();

    let skills = skill_dirs;

    // Create agent with a single context prompt
    let mut agent = client
        .agent(&model)
        .tool(BashTool)
        .preamble(SYSTEM_PROMPT)
        .build();

    // Piped mode: read all of stdin as a single prompt, run once, exit
    if !io::stdin().is_terminal() {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).ok();
        let input = input.trim();
        if input.is_empty() {
            eprintln!("No input on stdin.");
            std::process::exit(1);
        }

        eprintln!("{DIM}  batstone (piped mode) — model: {model}{RESET}");
        run_prompt(&mut agent, input).await;
        return Ok(());
    }

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".into());

    print_banner();
    println!("{DIM}  model: {model}{RESET}");
    if !skills.is_empty() {
        println!("{DIM}  skills: {} loaded{RESET}", skills.len());
    }
    println!("{DIM}  cwd:   {cwd}{RESET}\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("{BOLD}{GREEN}> {RESET}");
        io::stdout().flush().ok();

        let line = match lines.next() {
            Some(Ok(l)) => l,
            _ => break,
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        match input {
            "/quit" | "/exit" => break,
            _ => {}
        }

        run_prompt(&mut agent, input).await;
    }

    println!("\n{DIM}  bye 👋{RESET}\n");

    Ok(())
}

async fn run_prompt<T: CompletionModel>(agent: &mut Agent<T>, input: &str) {
    let result = agent.prompt(input).await.expect("prompt failed");
    println!("{}", result)
}

/// AgentSkills open standard skill set
struct SkillSet {}

struct ReadFileTool;

#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadFileToolArgs {
    pub path: String,
}
impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Error = ToolError;
    type Args = ReadFileToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Reads the contents of a file".to_string(),
            parameters: to_value(schema_for!(ReadFileToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        todo!()
    }
}

struct BashTool;

#[derive(Serialize, Deserialize, JsonSchema)]
/// Arguments to parse to the `BashTool`
///
/// * `command`: The command to run
/// * `cwd`: Working directory to run the command from (defaults to current directiry)
struct BashToolArgs {
    pub command: String,
    pub cwd: Option<String>,
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = ToolError;
    type Args = BashToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Runs a bash command and returns its stdout and stderr".to_string(),
            parameters: to_value(schema_for!(BashToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(&args.command);
        if let Some(cwd) = &args.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd
            .output()
            .await
            .map_err(|e| ToolError::ToolCallError(e.to_string().into()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(&stderr);
        }
        if exit_code != 0 {
            result.push_str(&format!("\n[exit code: {exit_code}]"));
        }

        Ok(result)
    }
}

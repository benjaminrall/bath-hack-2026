use rig::client::{CompletionClient, ProviderClient};
use rig::completion::Prompt;
use rig::providers::anthropic;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // Create OpenAI client
    let client = anthropic::Client::new("API KEY")?;

    // Create agent with a single context prompt
    let comedian_agent = client
        .agent("")
        .preamble("You are a comedian here to entertain the user using humour and jokes.")
        .build();

    // Prompt the agent and print the response
    let response = comedian_agent.prompt("Entertain me!").await?;

    println!("{response}");

    Ok(())
}
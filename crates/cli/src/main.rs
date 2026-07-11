use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser as _;
use yash_eventsctl::{event_stream, execute, format_result, Cli, Command, EventsCommand};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = if matches!(
        cli.command,
        Command::Events {
            command: EventsCommand::Follow
        }
    ) {
        follow(&cli).await
    } else {
        execute(&cli).await.and_then(|value| {
            println!("{}", format_result(&cli.command, &value, cli.json));
            if matches!(cli.command, Command::Replay { .. })
                && !value["metrics"]["passed"].as_bool().unwrap_or(false)
            {
                Err(yash_eventsctl::CliError::Replay(
                    "configured metric thresholds were not met".into(),
                ))
            } else {
                Ok(())
            }
        })
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "yash-eventsctl: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

async fn follow(cli: &Cli) -> Result<(), yash_eventsctl::CliError> {
    let mut client = event_stream(cli).await?;
    loop {
        let notification = client.next_notification().await?;
        if cli.json {
            println!(
                "{}",
                serde_json::to_string(&notification)
                    .map_err(yash_app_events_protocol::ClientError::from)?
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&notification)
                    .map_err(yash_app_events_protocol::ClientError::from)?
            );
        }
    }
}

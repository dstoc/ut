use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "ut", about = "Dictation control for Sway/Wayland")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum Command {
    Start,
    Stop,
    Abort,
    Toggle,
    Status,
    Health,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let invocation = match cli.command {
        Some(Command::Start) => ut::Invocation::Start,
        Some(Command::Stop) => ut::Invocation::Stop,
        Some(Command::Abort) => ut::Invocation::Abort,
        Some(Command::Toggle) | None => ut::Invocation::Toggle,
        Some(Command::Status) => ut::Invocation::Status,
        Some(Command::Health) => ut::Invocation::Health,
    };

    ut::run(invocation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_no_subcommand_as_toggle_default() {
        let cli = Cli::parse_from(["ut"]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_start_subcommand() {
        let cli = Cli::parse_from(["ut", "start"]);
        assert!(matches!(cli.command, Some(Command::Start)));
    }

    #[test]
    fn parses_stop_subcommand() {
        let cli = Cli::parse_from(["ut", "stop"]);
        assert!(matches!(cli.command, Some(Command::Stop)));
    }

    #[test]
    fn parses_health_subcommand() {
        let cli = Cli::parse_from(["ut", "health"]);
        assert!(matches!(cli.command, Some(Command::Health)));
    }
}

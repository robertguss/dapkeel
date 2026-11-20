use crate::error::DapError;
use clap::ValueEnum;
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Auto,
    Human,
    Agent,
    Json,
}

impl OutputFormat {
    pub fn resolve(self) -> ResolvedFormat {
        match self {
            Self::Auto => {
                if std::io::stdout().is_terminal() {
                    ResolvedFormat::Human
                } else {
                    ResolvedFormat::Agent
                }
            }
            Self::Human => ResolvedFormat::Human,
            Self::Agent => ResolvedFormat::Agent,
            Self::Json => ResolvedFormat::Json,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFormat {
    Human,
    Agent,
    Json,
}

impl ResolvedFormat {
    pub fn is_structured(self) -> bool {
        matches!(self, Self::Agent | Self::Json)
    }
}

pub fn print_structured<T: Serialize>(fmt: ResolvedFormat, value: &T, to_stderr: bool) {
    let rendered = match fmt {
        ResolvedFormat::Json => serde_json::to_string_pretty(value),
        ResolvedFormat::Agent | ResolvedFormat::Human => serde_json::to_string(value),
    }
    .unwrap_or_else(|_| "{}".to_string());

    if to_stderr {
        eprintln!("{rendered}");
    } else {
        println!("{rendered}");
    }
}

pub fn print_error(fmt: ResolvedFormat, err: &DapError) {
    match fmt {
        ResolvedFormat::Agent | ResolvedFormat::Json => {
            let payload = err.to_payload();
            print_structured(fmt, &payload, true);
        }
        ResolvedFormat::Human => {
            eprintln!("{} {}: {}", "error".red().bold(), err.code().yellow(), err);
            if let Some(hint) = err.fix_hint() {
                eprintln!("  {} {}", "hint:".cyan().bold(), hint);
            }
        }
    }
}

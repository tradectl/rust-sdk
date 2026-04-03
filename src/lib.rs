pub mod types;
pub mod strategy;
pub mod exchange;
pub mod runner;
pub mod bot_state;
#[cfg(feature = "monitor")]
pub mod monitor;
#[cfg(feature = "runner")]
mod run_cli;

// Re-export top-level for convenience
pub use types::*;
pub use strategy::*;

/// Entry point for self-contained strategy binaries.
///
/// Call from `main()` in your strategy project:
/// ```ignore
/// fn main() {
///     tradectl_sdk::run(|params| {
///         Box::new(MyStrategy::new(params))
///     });
/// }
/// ```
///
/// Handles `run` (paper/live), `backtest`, and `sweep` subcommands.
/// The `run` command is handled directly. Backtest/sweep print instructions
/// to use the `tradectl` CLI.
///
/// For embedded backtest/sweep support, use `run_with_handler()` and
/// integrate `tradectl-backtest` in your strategy crate.
///
/// Requires the `runner` feature.
#[cfg(feature = "runner")]
pub fn run(factory: StrategyFactory) {
    run_cli::run(factory);
}

/// Extended entry point that dispatches all commands to a custom handler.
///
/// Use this when your strategy binary depends on `tradectl-backtest` directly
/// and wants to handle backtest/sweep inline:
/// ```ignore
/// fn main() {
///     tradectl_sdk::run_with_handler(|cmd| match cmd {
///         RunCommand::Run(config) => { /* ... */ },
///         RunCommand::Backtest(args) => { /* ... */ },
///         RunCommand::Sweep(args) => { /* ... */ },
///     });
/// }
/// ```
///
/// Requires the `runner` feature.
#[cfg(feature = "runner")]
pub fn run_with_handler(handler: impl FnOnce(run_cli::RunCommand)) {
    run_cli::run_with_handler(handler);
}

#[cfg(feature = "runner")]
pub use run_cli::{RunCommand, BacktestRunArgs, SweepRunArgs};

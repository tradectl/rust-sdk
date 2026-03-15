//! `tradectl_sdk::run()` — entry point for self-contained strategy binaries.
//!
//! Strategy projects call this from `main()`. It parses the `run` subcommand
//! and dispatches to the appropriate mode (paper/live).
//!
//! Backtest and sweep subcommands are handled by the `tradectl` CLI, which
//! proxies to the strategy binary. Strategy binaries that want embedded
//! backtest/sweep support depend on `tradectl-backtest` directly.
//!
//! Requires the `runner` feature: `tradectl-sdk = { ..., features = ["runner"] }`

use clap::{Parser, Subcommand, Args};
use std::path::PathBuf;

use crate::strategy::StrategyFactory;
use crate::types::Params;

#[derive(Parser)]
#[command(about = "tradectl strategy binary")]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Run strategy live/paper with a config file
    Run(RunArgs),
    /// Run a single backtest (requires tradectl-backtest dependency)
    Backtest(BacktestArgs),
    /// Grid search over parameter ranges (requires tradectl-backtest dependency)
    Sweep(SweepArgs),
}

#[derive(Args)]
struct RunArgs {
    /// Path to bot config JSON
    #[arg(long)]
    config: PathBuf,
}

#[derive(Args)]
struct BacktestArgs {
    /// Path to prepared data file (.parquet or .bin)
    #[arg(short, long)]
    data: PathBuf,

    /// Initial balance in USD
    #[arg(long, default_value_t = 10_000.0)]
    balance: f64,

    /// Leverage multiplier
    #[arg(long, default_value_t = 1.0)]
    leverage: f64,

    /// Taker fee rate
    #[arg(long, default_value_t = 0.0004)]
    taker_fee: f64,

    /// Maker fee rate
    #[arg(long, default_value_t = 0.0002)]
    maker_fee: f64,

    /// Slippage percentage
    #[arg(long, default_value_t = 0.0001)]
    slippage: f64,

    /// Strategy parameters as key=value pairs (repeatable)
    #[arg(short, long, value_parser = parse_param)]
    param: Vec<(String, f64)>,

    /// Print individual trades
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Args)]
struct SweepArgs {
    /// Path to prepared data file (.parquet or .bin)
    #[arg(short, long)]
    data: PathBuf,

    /// Initial balance in USD
    #[arg(long, default_value_t = 10_000.0)]
    balance: f64,

    /// Leverage multiplier
    #[arg(long, default_value_t = 1.0)]
    leverage: f64,

    /// Taker fee rate
    #[arg(long, default_value_t = 0.0004)]
    taker_fee: f64,

    /// Maker fee rate
    #[arg(long, default_value_t = 0.0002)]
    maker_fee: f64,

    /// Slippage percentage
    #[arg(long, default_value_t = 0.0001)]
    slippage: f64,

    /// Parameter ranges as key=min:max:step (repeatable)
    #[arg(short, long, value_parser = parse_range)]
    range: Vec<ParamRange>,

    /// Fixed parameters as key=value (repeatable)
    #[arg(short, long, value_parser = parse_param)]
    param: Vec<(String, f64)>,

    /// Number of top results to show
    #[arg(long, default_value_t = 20)]
    top: usize,

    /// Minimum trades to consider
    #[arg(long, default_value_t = 5)]
    min_trades: usize,

    /// Save results as JSON to file
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct ParamRange {
    key: String,
    min: f64,
    max: f64,
    step: f64,
}

fn parse_param(s: &str) -> Result<(String, f64), String> {
    let (key, val) = s
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got '{s}'"))?;
    let value: f64 = val
        .parse()
        .map_err(|e| format!("invalid number '{val}': {e}"))?;
    Ok((key.to_string(), value))
}

fn parse_range(s: &str) -> Result<ParamRange, String> {
    let (key, rest) = s
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=MIN:MAX:STEP, got '{s}'"))?;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("expected MIN:MAX:STEP, got '{rest}'"));
    }
    Ok(ParamRange {
        key: key.to_string(),
        min: parts[0].parse().map_err(|e| format!("bad min: {e}"))?,
        max: parts[1].parse().map_err(|e| format!("bad max: {e}"))?,
        step: parts[2].parse().map_err(|e| format!("bad step: {e}"))?,
    })
}

/// Parsed backtest arguments, exposed for strategy binaries that integrate
/// `tradectl-backtest` directly.
pub struct BacktestRunArgs {
    pub data: PathBuf,
    pub balance: f64,
    pub leverage: f64,
    pub taker_fee: f64,
    pub maker_fee: f64,
    pub slippage: f64,
    pub params: Params,
    pub verbose: bool,
}

/// Parsed sweep arguments, exposed for strategy binaries that integrate
/// `tradectl-backtest` directly.
pub struct SweepRunArgs {
    pub data: PathBuf,
    pub balance: f64,
    pub leverage: f64,
    pub taker_fee: f64,
    pub maker_fee: f64,
    pub slippage: f64,
    pub fixed_params: Vec<(String, f64)>,
    pub ranges: Vec<(String, f64, f64, f64)>,
    pub top: usize,
    pub min_trades: usize,
    pub output: Option<PathBuf>,
}

/// Callback type for handling backtest/sweep commands in the strategy binary.
/// Return `true` if the command was handled, `false` to fall through to defaults.
pub enum RunCommand {
    Run(PathBuf),
    Backtest(BacktestRunArgs),
    Sweep(SweepRunArgs),
}

/// Entry point for strategy binaries. Call from `main()`:
///
/// ```ignore
/// fn main() {
///     tradectl_sdk::run(|params| {
///         Box::new(MyStrategy::new(params))
///     });
/// }
/// ```
///
/// Handles `run`, `backtest`, and `sweep` subcommands. The `run` command
/// is handled by the SDK (paper mode). Backtest and sweep print instructions
/// to use the `tradectl` CLI unless a custom handler is registered.
pub fn run(factory: StrategyFactory) {
    let cli = Cli::parse();

    match cli.command {
        CliCommand::Run(args) => run_live(args, factory),
        CliCommand::Backtest(args) => {
            eprintln!("Backtest mode is handled by the `tradectl` CLI.");
            eprintln!("Run: tradectl backtest --data {}", args.data.display());
            eprintln!();
            eprintln!("To embed backtest support directly in your strategy binary,");
            eprintln!("add `tradectl-backtest` to your Cargo.toml and use");
            eprintln!("`tradectl_sdk::run_with_backtest()` instead.");
            std::process::exit(1);
        }
        CliCommand::Sweep(args) => {
            eprintln!("Sweep mode is handled by the `tradectl` CLI.");
            eprintln!("Run: tradectl sweep --data {}", args.data.display());
            std::process::exit(1);
        }
    }
}

/// Extended entry point that dispatches all commands (run, backtest, sweep)
/// to the provided handler. Strategy binaries that depend on `tradectl-backtest`
/// use this to handle backtest/sweep inline.
pub fn run_with_handler(handler: impl FnOnce(RunCommand)) {
    let cli = Cli::parse();

    match cli.command {
        CliCommand::Run(args) => handler(RunCommand::Run(args.config)),
        CliCommand::Backtest(args) => {
            let mut params = Params::new();
            for (key, value) in &args.param {
                params = params.set(key, *value);
            }
            handler(RunCommand::Backtest(BacktestRunArgs {
                data: args.data,
                balance: args.balance,
                leverage: args.leverage,
                taker_fee: args.taker_fee,
                maker_fee: args.maker_fee,
                slippage: args.slippage,
                params,
                verbose: args.verbose,
            }));
        }
        CliCommand::Sweep(args) => {
            let ranges: Vec<_> = args
                .range
                .iter()
                .map(|r| (r.key.clone(), r.min, r.max, r.step))
                .collect();
            handler(RunCommand::Sweep(SweepRunArgs {
                data: args.data,
                balance: args.balance,
                leverage: args.leverage,
                taker_fee: args.taker_fee,
                maker_fee: args.maker_fee,
                slippage: args.slippage,
                fixed_params: args.param,
                ranges,
                top: args.top,
                min_trades: args.min_trades,
                output: args.output,
            }));
        }
    }
}

#[allow(unused_variables)]
fn run_live(args: RunArgs, factory: StrategyFactory) {
    let raw = std::fs::read_to_string(&args.config).unwrap_or_else(|e| {
        eprintln!("Failed to read config {}: {e}", args.config.display());
        std::process::exit(1);
    });

    let config: crate::types::config::BotConfig =
        serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("Invalid config: {e}");
            std::process::exit(1);
        });

    let all_paper = config.strats.iter().all(|s| s.is_emulator);

    if all_paper {
        #[cfg(feature = "paper")]
        {
            crate::paper::run(&args.config.to_string_lossy(), |strat| {
                let params = strat_entry_to_params(strat);
                factory(&params)
            });
            return;
        }

        #[cfg(not(feature = "paper"))]
        {
            eprintln!("Paper trading requires the 'paper' feature.");
            eprintln!(
                "Add to Cargo.toml: tradectl-sdk = {{ ..., features = [\"runner\", \"paper\"] }}"
            );
            std::process::exit(1);
        }
    } else {
        eprintln!("Live trading mode requires the tradectl-live runner.");
        eprintln!("This will be integrated in a future release.");
        std::process::exit(1);
    }

    #[allow(unreachable_code)]
    {
        let _ = config;
    }
}

#[cfg(feature = "paper")]
fn strat_entry_to_params(strat: &crate::types::config::StratEntry) -> Params {
    let mut params = Params::new();
    for (key, value) in &strat.params {
        if let Some(v) = value.as_f64() {
            params = params.set(key, v);
        }
    }
    params
}

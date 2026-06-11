use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use tokio::net::TcpListener;
use tracing::{info, warn};

use etherealdb::config::{Config, CrushConfig};
use etherealdb::generate::generate;
use etherealdb::infer::infer;
use etherealdb::server;
use etherealdb::shape::CrushThreshold;

#[derive(Parser)]
#[command(name = "etherealdb", version, about = "A database that isn't there.")]
struct Cli {
    /// Address for the Postgres-protocol listener.
    #[arg(long, default_value = "127.0.0.1:5432")]
    pg: String,

    /// Deterministic mode: the same query always returns the same garbage.
    #[arg(long)]
    seed: Option<u64>,

    /// Row-count band for queries without a LIMIT, as min:max.
    #[arg(long, default_value = "5:20", value_parser = parse_band)]
    rows: (usize, usize),

    /// Crush mode: bury clients that send unsafe queries (no columns / WHERE /
    /// LIMIT) under a torrent of type-correct rows.
    #[arg(long)]
    crush: bool,

    /// Max rows streamed per crushed query.
    #[arg(long, default_value_t = 5_000_000)]
    crush_rows: u64,

    /// Detect and log unsafe queries, but answer them normally.
    #[arg(long)]
    crush_warn_only: bool,

    /// Which missing-safety signals trigger a crush.
    #[arg(long, value_enum, default_value_t = Threshold::All3)]
    crush_threshold: Threshold,

    /// Max simultaneous crush streams; further unsafe queries answer normally.
    #[arg(long, default_value_t = 4)]
    crush_concurrency: usize,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Threshold {
    /// Crush on `SELECT *` / no column list.
    Star,
    /// Crush on a missing WHERE clause.
    Where,
    /// Crush on a missing LIMIT clause.
    Limit,
    /// Crush when any two signals are missing.
    Any2,
    /// Crush only when all three are missing (default).
    All3,
}

impl From<Threshold> for CrushThreshold {
    fn from(t: Threshold) -> Self {
        match t {
            Threshold::Star => CrushThreshold::Star,
            Threshold::Where => CrushThreshold::Where,
            Threshold::Limit => CrushThreshold::Limit,
            Threshold::Any2 => CrushThreshold::Any2,
            Threshold::All3 => CrushThreshold::All3,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what the inference engine makes of column names.
    Infer {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

fn parse_band(s: &str) -> Result<(usize, usize), String> {
    let (lo, hi) = s.split_once(':').ok_or("expected min:max")?;
    let lo: usize = lo.parse().map_err(|_| "bad min")?;
    let hi: usize = hi.parse().map_err(|_| "bad max")?;
    if lo > hi {
        return Err("min > max".into());
    }
    Ok((lo, hi))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    if let Some(Cmd::Infer { names }) = cli.cmd {
        let mut rng = match cli.seed {
            Some(s) => ChaCha8Rng::seed_from_u64(s),
            None => ChaCha8Rng::from_os_rng(),
        };
        for name in names {
            let st = infer(&name);
            let samples: Vec<String> = (0..3).map(|_| generate(st, &mut rng)).collect();
            println!("{name:<24} {:<14} {}", format!("{st:?}"), samples.join(" | "));
        }
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "etherealdb=info".into()),
        )
        .init();

    let cfg = Arc::new(Config {
        seed: cli.seed,
        rows_min: cli.rows.0,
        rows_max: cli.rows.1,
        crush: CrushConfig {
            enabled: cli.crush,
            max_rows: cli.crush_rows,
            warn_only: cli.crush_warn_only,
            threshold: cli.crush_threshold.into(),
            concurrency: cli.crush_concurrency.max(1),
        },
    });

    let listener = TcpListener::bind(&cli.pg).await?;
    info!("EtherealDB listening on {} (postgres protocol)", cli.pg);
    if let Some(seed) = cfg.seed {
        info!("deterministic mode, seed {seed}");
    }
    if cfg.crush.enabled {
        if cfg.crush.warn_only {
            info!("crush mode armed (warn-only): unsafe queries logged, not crushed");
        } else {
            warn!(
                max_rows = cfg.crush.max_rows,
                concurrency = cfg.crush.concurrency,
                "CRUSH MODE ARMED — unsafe queries will be buried in rows"
            );
        }
    }

    server::serve(listener, cfg).await;
    Ok(())
}

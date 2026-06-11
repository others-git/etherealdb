use clap::{Parser, Subcommand, ValueEnum};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use tokio::net::TcpListener;
use tracing::{info, warn};

use std::path::PathBuf;

use etherealdb::config::{Config, CrushConfig, GhostConfig};
use etherealdb::generate::{Gen, gen_value};
use etherealdb::infer::{Rules, infer_with, wire_type};
use etherealdb::server::{self, Proto, Shared};
use etherealdb::shape::CrushThreshold;
use etherealdb::theme::{self, ThemeData};

#[derive(Parser)]
#[command(name = "etherealdb", version, about = "A database that isn't there.")]
struct Cli {
    /// Address for the Postgres-protocol listener.
    #[arg(long, default_value = "127.0.0.1:5432")]
    pg: String,

    /// Also listen for the MySQL protocol on this address (off by default).
    #[arg(long)]
    mysql: Option<String>,

    /// Also listen for the Redis (RESP) protocol on this address (off by default).
    #[arg(long)]
    redis: Option<String>,

    /// Deterministic mode: the same query always returns the same garbage.
    #[arg(long, global = true)]
    seed: Option<u64>,

    /// Row-count band for queries without a LIMIT, as min:max.
    #[arg(long, default_value = "5:20", value_parser = parse_band)]
    rows: (usize, usize),

    /// Vocabulary theme for generated values (generic, ecommerce, finance, iot, users).
    #[arg(long, default_value = "generic", value_parser = parse_theme, global = true)]
    theme: &'static ThemeData,

    /// File of custom inference rules, layered over the built-ins.
    #[arg(long, global = true)]
    rules: Option<PathBuf>,

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

    // --- ghosts: fault injection + value fuzzing for client resilience tests ---
    /// Chance [0..1] of delaying a response (haunting latency).
    #[arg(long, default_value_t = 0.0)]
    ghost_latency: f64,

    /// Latency band in ms when a delay is injected, as min:max.
    #[arg(long, default_value = "50:500", value_parser = parse_band_u64)]
    ghost_latency_ms: (u64, u64),

    /// Chance [0..1] of answering a command with a protocol error.
    #[arg(long, default_value_t = 0.0)]
    ghost_errors: f64,

    /// Chance [0..1] of dropping the connection mid-conversation.
    #[arg(long, default_value_t = 0.0)]
    ghost_drops: f64,

    /// Chance [0..1], per value, of emitting a pathological value to fuzz the client.
    #[arg(long, default_value_t = 0.0, global = true)]
    fuzz: f64,

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

fn parse_band_u64(s: &str) -> Result<(u64, u64), String> {
    let (lo, hi) = parse_band(s)?;
    Ok((lo as u64, hi as u64))
}

fn parse_theme(s: &str) -> Result<&'static ThemeData, String> {
    theme::by_name(s)
        .ok_or_else(|| format!("unknown theme `{s}` (try: {})", theme::names().join(", ")))
}

/// Load custom inference rules from a file, exiting with a clear message on error.
fn load_rules(path: Option<&PathBuf>) -> Rules {
    let Some(path) = path else {
        return Rules::default();
    };
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("etherealdb: cannot read rules file {}: {e}", path.display());
        std::process::exit(2);
    });
    Rules::parse(&text).unwrap_or_else(|e| {
        eprintln!("etherealdb: invalid rules file {}: {e}", path.display());
        std::process::exit(2);
    })
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    if let Some(Cmd::Infer { names }) = &cli.cmd {
        let rules = load_rules(cli.rules.as_ref());
        let g = Gen {
            theme: cli.theme,
            fuzz: cli.fuzz,
        };
        let mut rng = match cli.seed {
            Some(s) => ChaCha8Rng::seed_from_u64(s),
            None => ChaCha8Rng::from_os_rng(),
        };
        for name in names {
            let st = infer_with(name, &rules);
            let wt = wire_type(st);
            let samples: Vec<String> = (0..3).map(|_| gen_value(st, wt, &mut rng, g)).collect();
            println!(
                "{name:<24} {:<14} {}",
                format!("{st:?}"),
                samples.join(" | ")
            );
        }
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "etherealdb=info".into()),
        )
        .init();

    let rules = load_rules(cli.rules.as_ref());
    let cfg = Config {
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
        theme: cli.theme,
        rules,
        ghosts: GhostConfig {
            latency_prob: cli.ghost_latency,
            latency_ms: cli.ghost_latency_ms,
            error_prob: cli.ghost_errors,
            drop_prob: cli.ghost_drops,
            fuzz: cli.fuzz,
        },
    };

    if let Some(seed) = cfg.seed {
        info!("deterministic mode, seed {seed}");
    }
    if cfg.theme.name != "generic" {
        info!("theme: {}", cfg.theme.name);
    }
    if !cfg.rules.is_empty() {
        info!("loaded {} custom inference rule(s)", cfg.rules.len());
    }
    if cfg.ghosts.haunting() || cfg.ghosts.fuzz > 0.0 {
        warn!(
            latency = cfg.ghosts.latency_prob,
            errors = cfg.ghosts.error_prob,
            drops = cfg.ghosts.drop_prob,
            fuzz = cfg.ghosts.fuzz,
            "👻 ghosts are loose — clients will see latency, errors, drops, and/or junk"
        );
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

    let shared = Shared::new(cfg);

    let pg_listener = TcpListener::bind(&cli.pg).await?;
    info!("EtherealDB listening on {} (postgres protocol)", cli.pg);
    let pg = tokio::spawn(server::run(pg_listener, shared.clone(), Proto::Postgres));

    let mut extra = Vec::new();
    if let Some(addr) = &cli.mysql {
        let l = TcpListener::bind(addr).await?;
        info!("EtherealDB listening on {addr} (mysql protocol)");
        extra.push(tokio::spawn(server::run(l, shared.clone(), Proto::Mysql)));
    }
    if let Some(addr) = &cli.redis {
        let l = TcpListener::bind(addr).await?;
        info!("EtherealDB listening on {addr} (redis protocol)");
        extra.push(tokio::spawn(server::run(l, shared.clone(), Proto::Redis)));
    }

    // All accept loops run forever; surface a panic from any of them.
    pg.await.ok();
    for task in extra {
        task.await.ok();
    }
    Ok(())
}

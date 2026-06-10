use std::sync::Arc;

use clap::{Parser, Subcommand};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use tokio::net::TcpListener;
use tracing::info;

use etherealdb::config::Config;
use etherealdb::generate::generate;
use etherealdb::infer::infer;
use etherealdb::server;

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

    #[command(subcommand)]
    cmd: Option<Cmd>,
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
    });

    let listener = TcpListener::bind(&cli.pg).await?;
    info!("EtherealDB listening on {} (postgres protocol)", cli.pg);
    if let Some(seed) = cfg.seed {
        info!("deterministic mode, seed {seed}");
    }

    server::serve(listener, cfg).await;
    Ok(())
}

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::config::Config;
use crate::proto;

/// Shared per-server state handed to every connection.
pub struct Shared {
    pub cfg: Config,
    /// Caps simultaneous crush streams. Unsafe queries that can't acquire a
    /// permit get an ordinary response instead.
    pub crush_slots: Semaphore,
}

impl Shared {
    pub fn new(cfg: Config) -> Arc<Self> {
        let permits = cfg.crush.concurrency.max(1);
        Arc::new(Self {
            cfg,
            crush_slots: Semaphore::new(permits),
        })
    }
}

/// Which wire protocol a listener speaks.
#[derive(Clone, Copy, Debug)]
pub enum Proto {
    Postgres,
    Mysql,
    Redis,
}

/// Accept loop: each connection is handled by the frontend for `proto`.
pub async fn run(listener: TcpListener, shared: Arc<Shared>, proto: Proto) {
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let res = match proto {
                        Proto::Postgres => proto::pg::handle(sock, shared).await,
                        Proto::Mysql => proto::mysql::handle(sock, shared).await,
                        Proto::Redis => proto::resp::handle(sock, shared).await,
                    };
                    if let Err(e) = res {
                        debug!(%peer, ?proto, "connection ended: {e}");
                    }
                });
            }
            Err(e) => warn!("accept failed: {e}"),
        }
    }
}

/// Convenience wrapper: serve a single Postgres listener (used by tests).
pub async fn serve(listener: TcpListener, cfg: Arc<Config>) {
    let shared = Shared::new((*cfg).clone());
    run(listener, shared, Proto::Postgres).await;
}

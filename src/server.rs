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
        Arc::new(Self { cfg, crush_slots: Semaphore::new(permits) })
    }
}

pub async fn serve(listener: TcpListener, cfg: Arc<Config>) {
    let shared = Shared::new((*cfg).clone());
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = proto::pg::handle(sock, shared).await {
                        debug!(%peer, "connection ended: {e}");
                    }
                });
            }
            Err(e) => warn!("accept failed: {e}"),
        }
    }
}

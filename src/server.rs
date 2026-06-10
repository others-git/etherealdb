use std::sync::Arc;

use tokio::net::TcpListener;
use tracing::{debug, warn};

use crate::config::Config;
use crate::proto;

pub async fn serve(listener: TcpListener, cfg: Arc<Config>) {
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    if let Err(e) = proto::pg::handle(sock, cfg).await {
                        debug!(%peer, "connection ended: {e}");
                    }
                });
            }
            Err(e) => warn!("accept failed: {e}"),
        }
    }
}

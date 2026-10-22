//! The plain HTTP listener serving mirror content (ADR-0030). The router
//! is shared with relays (ADR-0034) in [`monitorflow_common::mirror::serve`],
//! so both serve the same layout and `/index.json`; this module maps the
//! server's [`Layout`] onto it.

use std::future::Future;

use axum::Router;
use monitorflow_common::mirror::{serve, Served};
use tokio::net::TcpListener;

use super::layout::Layout;
use super::model::Family;

/// What the server serves: aptly's `public/`, the dnf snapshots and the
/// public key.
pub fn served(layout: &Layout) -> Served {
    Served {
        apt: layout.served(Family::Apt),
        dnf: layout.served(Family::Dnf),
        public_key: layout.public_key(),
    }
}

/// The routes over `layout`.
pub fn router(layout: &Layout) -> Router {
    serve::router(&served(layout))
}

/// Serves [`router`] on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    layout: Layout,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve::serve(listener, served(&layout), shutdown).await
}

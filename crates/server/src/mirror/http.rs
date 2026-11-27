//! The plain HTTP listener serving mirror content (ADR-0030). The router
//! is shared with relays (ADR-0034) in [`monitorflow_common::mirror::serve`],
//! so both serve the same layout and `/index.json`; this module maps the
//! server's [`Layout`] onto it.

use std::future::Future;

use monitorflow_common::mirror::demand::OnDemand;
use monitorflow_common::mirror::{serve, Served};
use tokio::net::TcpListener;

use super::layout::Layout;
use super::model::Family;

/// What the server serves: aptly's `public/`, the dnf snapshots and the
/// public key.
pub fn served(layout: &Layout) -> Served {
    Served {
        base: layout.root().to_owned(),
        apt: layout.served(Family::Apt),
        dnf: layout.served(Family::Dnf),
        public_key: layout.public_key(),
        agent: layout.root().join(crate::agent_releases::DIR),
        software: layout.root().join(crate::software_files::DIR),
    }
}

/// Serves the mirror content of `layout` on `listener` until `shutdown` resolves, fetching the
/// packages of on-demand snapshots with `ondemand` (ADR-0142). Downloads
/// a stopped server left half done are removed first.
pub async fn serve(
    listener: TcpListener,
    layout: Layout,
    ondemand: Option<OnDemand>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let store = layout.store();
    tokio::task::spawn_blocking(move || store.prepare())
        .await
        .map_err(std::io::Error::other)??;
    serve::serve_with(listener, served(&layout), ondemand, shutdown).await
}

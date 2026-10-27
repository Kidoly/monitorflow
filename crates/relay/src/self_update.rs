//! Relay self-update (ADR-0055): the agent's self-update, run for the
//! relay's own binary and unit ([`RELAY`]).
//!
//! The server sends a signed `self_update` command addressed to the relay
//! on the relay's own stream. The agent's [`Dispatcher`] checks it (the
//! signature by the CA, the relay's ID, the expiry, the journal), the
//! agent's handler downloads and verifies the release and hands it to
//! `sudo -n /usr/bin/monitorflow-relay self-update`, whose watchdog
//! restarts `monitorflow-relay` and puts the previous binary back unless
//! the new relay's stream is accepted upstream in time: the relay writes
//! its [`Connected`] file each time it is. The relay runs no other kind of
//! command.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_agent::commands::{
    self, Dispatcher, DispatcherEnd, DispatcherSettings, Handlers, Journal, StreamEnd,
};
use monitorflow_agent::self_update::handler::{SelfUpdateHandler, SudoHelper};
use monitorflow_agent::self_update::{Connected, StatusResumer, RELAY};
use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::command::SELF_UPDATE;
use rustls::pki_types::CertificateDer;

/// The dispatcher of the relay's own commands, and the end its stream
/// holds.
pub fn dispatcher(
    relay_id: &str,
    ca_certs: Vec<CertificateDer<'static>>,
    data_dir: &Path,
    handler: SelfUpdateHandler,
    state_dir: &Path,
) -> (Dispatcher, StreamEnd, DispatcherEnd) {
    let mut handlers = Handlers::default();
    handlers.register(SELF_UPDATE, Arc::new(handler));
    let dispatcher = Dispatcher::new(
        relay_id,
        ca_certs,
        handlers,
        Journal::in_dir(data_dir),
        DispatcherSettings::default(),
    )
    .with_resumer(Arc::new(StatusResumer::new(state_dir)));
    let (stream, end) = commands::link();
    (dispatcher, stream, end)
}

/// The real system's: the release keys built in, the root helper through
/// sudo, root's state directory.
pub fn start(
    relay_id: &str,
    ca_certs: Vec<CertificateDer<'static>>,
    data_dir: &Path,
    shutdown: Shutdown,
) -> StreamEnd {
    let handler = SelfUpdateHandler::new(SudoHelper::of(RELAY)).for_product(RELAY);
    let (dispatcher, stream, end) = dispatcher(
        relay_id,
        ca_certs,
        data_dir,
        handler,
        Path::new(RELAY.state_dir),
    );
    tokio::spawn(dispatcher.run(end, shutdown));
    stream
}

/// Records, off the runtime, that this relay's stream was accepted: the
/// watchdog's proof that a new version works.
pub fn record_connected(data_dir: PathBuf) {
    tokio::task::spawn_blocking(move || {
        if let Err(error) = Connected::record(&data_dir) {
            tracing::warn!(%error, "cannot record the connection for self-updates");
        }
    });
}

#[cfg(test)]
mod tests;

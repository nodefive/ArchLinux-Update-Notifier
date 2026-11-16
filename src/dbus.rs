//! Session-bus interface.
//!
//!   bus name   io.github.nodefive.ArchUpdateNotifier
//!   object     /io/github/nodefive/ArchUpdateNotifier
//!   interface  io.github.nodefive.ArchUpdateNotifier1
//!     CheckNow() -> (b accepted)
//!     signal UpdatesChanged(u count, x last_check)
//!     property LastCheck x, Count u, Status s   (PropertiesChanged emitted)

use crate::state::State;
use tokio::sync::mpsc;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::{interface, Connection};

pub const BUS_NAME: &str = "io.github.nodefive.ArchUpdateNotifier";
pub const OBJECT_PATH: &str = "/io/github/nodefive/ArchUpdateNotifier";

#[derive(Debug)]
pub enum Command {
    CheckNow,
}

pub struct Service {
    tx: mpsc::Sender<Command>,
    last_check: i64,
    count: u32,
    status: String,
}

#[interface(name = "io.github.nodefive.ArchUpdateNotifier1")]
impl Service {
    /// Request an immediate check. A request while a check is running is
    /// queued (at most one). Returns false only while shutting down.
    #[zbus(out_args("accepted"))]
    async fn check_now(&self) -> bool {
        match self.tx.try_send(Command::CheckNow) {
            Ok(()) => true,
            // Queue full: a check request is already pending, which is fine.
            Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    #[zbus(property)]
    fn last_check(&self) -> i64 {
        self.last_check
    }

    #[zbus(property)]
    fn count(&self) -> u32 {
        self.count
    }

    #[zbus(property)]
    fn status(&self) -> String {
        self.status.clone()
    }

    #[zbus(signal)]
    async fn updates_changed(emitter: &SignalEmitter<'_>, count: u32, last_check: i64) -> zbus::Result<()>;
}

pub struct Bus {
    // Kept alive for the lifetime of the daemon.
    _conn: Connection,
    iface: InterfaceRef<Service>,
}

impl Bus {
    pub async fn start(tx: mpsc::Sender<Command>, state: &State) -> anyhow::Result<Bus> {
        let svc = Service {
            tx,
            last_check: state.last_check,
            count: state.count,
            status: state.status.as_str().to_string(),
        };
        let conn = zbus::connection::Builder::session()
            .serve_at(OBJECT_PATH, svc)
            .name(BUS_NAME)
            .build()
            .await
            .map_err(|e| match e {
                zbus::Error::NameTaken => anyhow::anyhow!("{BUS_NAME} is already owned: another instance is running"),
                other => anyhow::anyhow!("cannot connect to the session bus: {other}"),
            })?;
        let iface = conn.object_server().interface::<_, Service>(OBJECT_PATH).await?;
        Ok(Bus { _conn: conn, iface })
    }

    /// Sync properties from the state, emitting PropertiesChanged for the
    /// ones that changed, and optionally the UpdatesChanged signal.
    pub async fn publish(&self, state: &State, updates_changed: bool) -> zbus::Result<()> {
        let emitter = self.iface.signal_emitter();
        {
            let mut svc = self.iface.get_mut().await;
            if svc.count != state.count {
                svc.count = state.count;
                svc.count_changed(emitter).await?;
            }
            if svc.last_check != state.last_check {
                svc.last_check = state.last_check;
                svc.last_check_changed(emitter).await?;
            }
            let status = state.status.as_str();
            if svc.status != status {
                svc.status = status.to_string();
                svc.status_changed(emitter).await?;
            }
        }
        if updates_changed {
            Service::updates_changed(emitter, state.count, state.last_check).await?;
        }
        Ok(())
    }
}

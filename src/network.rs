//! Network state from NetworkManager over the system bus. Without
//! NetworkManager the state is `Unknown` and checks proceed normally.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetState {
    Online { metered: bool },
    Offline,
    Unknown,
}

// NMMetered: 1 = yes, 3 = guess-yes.
const NM_METERED_YES: u32 = 1;
const NM_METERED_GUESS_YES: u32 = 3;
// NMConnectivityState: 1 = none.
const NM_CONNECTIVITY_NONE: u32 = 1;

pub async fn query() -> NetState {
    match tokio::time::timeout(Duration::from_secs(5), query_nm()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "NetworkManager not available");
            NetState::Unknown
        }
        Err(_) => {
            tracing::debug!("NetworkManager query timed out");
            NetState::Unknown
        }
    }
}

async fn query_nm() -> zbus::Result<NetState> {
    let conn = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await?;
    let connectivity: u32 = proxy.get_property("Connectivity").await?;
    if connectivity == NM_CONNECTIVITY_NONE {
        return Ok(NetState::Offline);
    }
    let metered: u32 = proxy.get_property("Metered").await?;
    Ok(NetState::Online {
        metered: metered == NM_METERED_YES || metered == NM_METERED_GUESS_YES,
    })
}

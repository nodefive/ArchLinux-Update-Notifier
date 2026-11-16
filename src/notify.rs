//! Desktop notification via org.freedesktop.Notifications (notify-rust).

use crate::config::Source;
use crate::state::State;
use notify_rust::{Hint, Notification, Timeout};

pub fn summary_text(state: &State) -> (String, String) {
    let noun = if state.count == 1 { "update" } else { "updates" };
    let title = format!("{} {} available", state.count, noun);
    let parts: Vec<String> = Source::ALL
        .iter()
        .filter_map(|s| {
            let n = state.counts.get(s).copied().unwrap_or(0);
            (n > 0).then(|| format!("{n} {}", s.label()))
        })
        .collect();
    (title, parts.join(", "))
}

pub async fn send(state: &State) -> anyhow::Result<()> {
    let (title, body) = summary_text(state);
    Notification::new()
        .appname("Arch Update Notifier")
        .summary(&title)
        .body(&body)
        .icon("software-update-available-symbolic")
        .hint(Hint::Category("system".into()))
        .timeout(Timeout::Default)
        .show_async()
        .await?;
    Ok(())
}

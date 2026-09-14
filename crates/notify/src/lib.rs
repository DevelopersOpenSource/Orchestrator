//! Notificações desktop cross-platform do Orchestrator.
//!
//! Linux: D-Bus (via `notify-rust`). Windows: WinRT toast (também via
//! `notify-rust`). Falha de notificação nunca derruba o chamador: o erro
//! é logado e engolido em [`notify_best_effort`].

use anyhow::Result;

/// Urgência da notificação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    /// Decisão crítica: fica na tela até o usuário interagir (Linux).
    Critical,
}

/// Envia uma notificação desktop.
pub fn notify(title: &str, body: &str, urgency: Urgency) -> Result<()> {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("Orchestrator");
    #[cfg(target_os = "linux")]
    {
        n.urgency(match urgency {
            Urgency::Normal => notify_rust::Urgency::Normal,
            Urgency::Critical => notify_rust::Urgency::Critical,
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = urgency;
    n.show()?;
    Ok(())
}

/// Como [`notify`], mas nunca retorna erro (apenas loga).
pub fn notify_best_effort(title: &str, body: &str, urgency: Urgency) {
    if let Err(err) = notify(title, body, urgency) {
        tracing::warn!(%err, "falha ao enviar notificação desktop");
    }
}

/// Atalho para o caso principal: decisão crítica aguardando na TUI.
pub fn notify_pending_decision(project: &str, summary: &str) {
    notify_best_effort(
        &format!("Orchestrator — decisão pendente [{project}]"),
        &format!("{summary}\n\nAbra `orchestrator tui` para decidir."),
        Urgency::Critical,
    );
}

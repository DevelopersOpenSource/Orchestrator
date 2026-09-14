//! Máquina de estados de uma sessão de projeto.

use serde::{Deserialize, Serialize};

use crate::config::ProjectConfig;
use crate::error::CoreError;

/// Estado de uma sessão de agente em um projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    /// Nenhuma sessão ativa.
    Idle,
    /// Sessão do agente em execução.
    Running,
    /// O agente parou e espera uma decisão humana.
    AwaitingDecision { question: String },
    /// A sessão terminou com sucesso.
    Done,
    /// A sessão falhou.
    Failed,
}

impl SessionState {
    fn label(&self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Running => "running",
            SessionState::AwaitingDecision { .. } => "awaiting_decision",
            SessionState::Done => "done",
            SessionState::Failed => "failed",
        }
    }

    /// Estado terminal (`Done`/`Failed`) ou não.
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionState::Done | SessionState::Failed)
    }

    /// Verifica se a transição `self -> next` é permitida.
    ///
    /// Regras:
    /// - `Idle -> Running`
    /// - `Running -> AwaitingDecision | Done | Failed`
    /// - `AwaitingDecision -> Running | Failed`
    /// - `Done | Failed -> Idle` (reset para nova sessão)
    pub fn can_transition_to(&self, next: &SessionState) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Idle, Running)
                | (Running, AwaitingDecision { .. })
                | (Running, Done)
                | (Running, Failed)
                | (AwaitingDecision { .. }, Running)
                | (AwaitingDecision { .. }, Failed)
                | (Done, Idle)
                | (Failed, Idle)
        )
    }
}

/// Um projeto em tempo de execução: configuração + estado de sessão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub config: ProjectConfig,
    pub state: SessionState,
}

impl Project {
    pub fn new(config: ProjectConfig) -> Self {
        Project {
            config,
            state: SessionState::Idle,
        }
    }

    /// Aplica uma transição, rejeitando as inválidas.
    pub fn transition(&mut self, next: SessionState) -> Result<(), CoreError> {
        if self.state.can_transition_to(&next) {
            self.state = next;
            Ok(())
        } else {
            Err(CoreError::InvalidTransition {
                from: self.state.label().to_string(),
                to: next.label().to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project() -> Project {
        Project::new(ProjectConfig {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            goal: "g".into(),
            cli: "claude_code".into(),
        })
    }

    #[test]
    fn happy_path() {
        let mut p = project();
        p.transition(SessionState::Running).unwrap();
        p.transition(SessionState::AwaitingDecision {
            question: "usar sqlite?".into(),
        })
        .unwrap();
        p.transition(SessionState::Running).unwrap();
        p.transition(SessionState::Done).unwrap();
        assert!(p.state.is_terminal());
        p.transition(SessionState::Idle).unwrap();
    }

    #[test]
    fn invalid_transition_rejected() {
        let mut p = project();
        let err = p.transition(SessionState::Done).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { .. }));
        assert_eq!(p.state, SessionState::Idle);
    }

    #[test]
    fn serde_roundtrip() {
        let s = SessionState::AwaitingDecision {
            question: "q?".into(),
        };
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains("awaiting_decision"));
        let back: SessionState = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);
    }
}

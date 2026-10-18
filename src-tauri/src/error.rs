use serde::Serialize;

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// Impossible de joindre le serveur / négociation SSH.
    #[error("{0}")]
    Connect(String),
    /// Échec d'authentification (ou clé d'hôte refusée).
    #[error("{0}")]
    Auth(String),
    /// Saisie annulée par l'utilisateur.
    #[error("{0}")]
    Cancelled(String),
    /// Erreur renvoyée par le serveur (SFTP, commande shell, SCP).
    #[error("{0}")]
    Remote(String),
    #[error("{0}")]
    Io(String),
    #[error("Transfert annulé")]
    TransferCancelled,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<russh::Error> for Error {
    fn from(e: russh::Error) -> Self {
        Error::Connect(format!("SSH : {e}"))
    }
}

impl From<russh_sftp::client::error::Error> for Error {
    fn from(e: russh_sftp::client::error::Error) -> Self {
        use russh_sftp::client::error::Error as E;
        match e {
            E::Status(s) => Error::Remote(if s.error_message.is_empty() {
                s.status_code.to_string()
            } else {
                s.error_message
            }),
            other => Error::Remote(format!("SFTP : {other}")),
        }
    }
}

/// Erreur transmise à l'interface (type + message).
#[derive(Debug, Clone, Serialize)]
pub struct CmdError {
    pub kind: &'static str,
    pub message: String,
}

impl From<Error> for CmdError {
    fn from(e: Error) -> Self {
        let kind = match e {
            Error::Connect(_) => "connect",
            Error::Auth(_) => "auth",
            Error::Cancelled(_) => "cancelled",
            Error::Remote(_) => "remote",
            Error::Io(_) => "io",
            Error::TransferCancelled => "cancelled",
        };
        CmdError {
            kind,
            message: e.to_string(),
        }
    }
}

impl From<String> for CmdError {
    fn from(message: String) -> Self {
        CmdError {
            kind: "other",
            message,
        }
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        CmdError {
            kind: "io",
            message: e.to_string(),
        }
    }
}

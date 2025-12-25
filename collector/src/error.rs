use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConnectorError {
    #[error("SerdeError: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("format error")]
    FormatError,
    #[error("connection abort")]
    ConnectionAbort,
    #[error("IPC error: {0}")]
    IpcError(String),
    #[error("Channel error: {0}")]
    ChannelError(String),
}

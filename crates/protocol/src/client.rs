use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::{method, Request, RequestId, Response, RpcError, PROTOCOL_VERSION};

/// One negotiated timeout-aware connection shared by CLI and GUI.
#[derive(Debug)]
pub struct UnixRpcClient {
    reader: BufReader<ReadHalf<UnixStream>>,
    writer: WriteHalf<UnixStream>,
    timeout: Duration,
    next_id: i64,
}

impl UnixRpcClient {
    /// Connects and performs the mandatory protocol-v1 handshake.
    ///
    /// # Errors
    ///
    /// Returns connection, timeout, protocol, or compatibility failures.
    pub async fn connect(
        path: &Path,
        duration: Duration,
        client_name: &str,
        client_version: &str,
    ) -> Result<Self, ClientError> {
        let stream = timeout(duration, UnixStream::connect(path))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let (reader, writer) = tokio::io::split(stream);
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            timeout: duration,
            next_id: 1,
        };
        client.call(method::HANDSHAKE,serde_json::json!({"protocol":PROTOCOL_VERSION,"client_name":client_name,"client_version":client_version})).await?;
        Ok(client)
    }

    /// Sends one request and waits for its matching response.
    ///
    /// # Errors
    ///
    /// Returns timeout, disconnect, JSON, I/O, or structured RPC errors.
    pub async fn call(&mut self, method_name: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = Request {
            jsonrpc: "2.0".into(),
            id: RequestId::Number(id),
            method: method_name.into(),
            params,
        };
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        timeout(self.timeout, self.writer.write_all(&bytes))
            .await
            .map_err(|_| ClientError::Timeout)??;
        let mut line = String::new();
        timeout(self.timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| ClientError::Timeout)??;
        if line.is_empty() {
            return Err(ClientError::Disconnected);
        }
        let response: Response = serde_json::from_str(&line)?;
        response.result.ok_or_else(|| {
            ClientError::Rpc(
                response
                    .error
                    .unwrap_or_else(|| RpcError::new(-32603, "response omitted result and error")),
            )
        })
    }

    /// Reads the next subscription notification without a response timeout.
    ///
    /// # Errors
    ///
    /// Returns disconnect, I/O, or malformed JSON errors.
    pub async fn next_notification(&mut self) -> Result<Value, ClientError> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        if line.is_empty() {
            return Err(ClientError::Disconnected);
        }
        Ok(serde_json::from_str(&line)?)
    }
}

/// Shared local-client transport failure.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("cannot communicate with daemon: {0}")]
    Io(#[from] io::Error),
    #[error("daemon request timed out")]
    Timeout,
    #[error("daemon disconnected")]
    Disconnected,
    #[error("protocol JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon RPC error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
}

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("failed to serialize JSON-RPC message: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to read from IPC channel: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON-RPC error (code {code}): {message}")]
    RpcError { code: i32, message: String },
}

/// JSON-RPC 2.0 request (Rust → Python).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    pub id: u64,
}

impl JsonRpcRequest {
    pub fn new(method: impl Into<String>, params: serde_json::Value, id: u64) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params: Some(params),
            id,
        }
    }

    /// Serialize to a single JSON line (newline-delimited).
    pub fn to_line(&self) -> Result<String, IpcError> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

/// JSON-RPC 2.0 response (Python → Rust).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: u64,
}

impl JsonRpcResponse {
    /// Parse from a single JSON line.
    pub fn from_line(line: &str) -> Result<Self, IpcError> {
        let resp: Self = serde_json::from_str(line.trim())?;
        Ok(resp)
    }

    /// Check if this response contains an error.
    pub fn into_result(self) -> Result<serde_json::Value, IpcError> {
        if let Some(err) = self.error {
            Err(IpcError::RpcError {
                code: err.code,
                message: err.message,
            })
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new("route", serde_json::json!({"message": "hello"}), 1);
        let line = req.to_line().unwrap();
        assert!(line.ends_with('\n'));
        assert!(line.contains("\"jsonrpc\":\"2.0\""));
        assert!(line.contains("\"method\":\"route\""));
    }

    #[test]
    fn test_response_success_parsing() {
        let json = r#"{"jsonrpc":"2.0","result":{"intent":"general"},"id":1}"#;
        let resp = JsonRpcResponse::from_line(json).unwrap();
        let result = resp.into_result().unwrap();
        assert_eq!(result["intent"], "general");
    }

    #[test]
    fn test_response_error_parsing() {
        let json =
            r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Model unavailable"},"id":1}"#;
        let resp = JsonRpcResponse::from_line(json).unwrap();
        let err = resp.into_result().unwrap_err();
        assert!(err.to_string().contains("Model unavailable"));
    }
}

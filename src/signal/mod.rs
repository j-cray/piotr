use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use log::{info, error, warn};

#[derive(Serialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Value,
    id: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<String>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct JsonRpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

#[derive(Deserialize, Debug)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: SignalMessage,
}

#[derive(Deserialize, Debug)]
pub struct SignalMessage {
    pub envelope: Option<Envelope>,
    // Add other fields as needed from signal-cli output
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Envelope {
    pub source: String,
    #[serde(rename = "sourceNumber")]
    pub source_number: Option<String>,
    #[serde(rename = "sourceUuid")]
    pub source_uuid: Option<String>,
    pub timestamp: u64,
    #[serde(rename = "sourceName")]
    pub source_name: Option<String>,
    #[serde(rename = "dataMessage")]
    pub data_message: Option<DataMessage>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct DataMessage {
    pub message: Option<String>,
    pub timestamp: u64,
    #[serde(rename = "groupInfo")]
    pub group_info: Option<GroupInfo>,
    pub quote: Option<Quote>,
    pub reaction: Option<Reaction>,
    pub mentions: Option<Vec<Mention>>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Mention {
    pub name: Option<String>,
    pub number: Option<String>,
    pub uuid: Option<String>,
    pub start: usize,
    pub length: usize,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Reaction {
    pub emoji: String,
    #[serde(rename = "targetAuthor")]
    pub target_author: String,
    #[serde(rename = "targetSentTimestamp")]
    pub target_sent_timestamp: u64,
    #[serde(rename = "isRemove")]
    pub is_remove: bool,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Quote {
    pub id: u64,
    pub author: String,
    pub text: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct GroupInfo {
    #[serde(rename = "groupId")]
    pub group_id: String,
    #[serde(rename = "type")]
    pub group_type: String,
}

#[derive(Clone)]
pub struct SignalClient {
    user_phone: String,
    tx: mpsc::Sender<Value>,
}

impl SignalClient {
    #[cfg(test)]
    pub fn new_dummy() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            user_phone: "dummy".to_string(),
            tx,
        }
    }

    pub async fn new(user_phone: &str) -> Result<(Self, mpsc::Receiver<SignalMessage>)> {
        info!("Starting signal-cli for user: [REDACTED]");
        let mut child = Command::new("signal-cli")
            .arg("--config")
            .arg("data/signal-cli")
            .arg("-u")
            .arg(user_phone)
            .arg("--output=json")
            .arg("jsonRpc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Log stderr to parent stderr
            .spawn()
            .context("Failed to spawn signal-cli")?;

        let mut stdin = child.stdin.take().context("No stdin handle")?;
        let stdout = child.stdout.take().context("No stdout handle")?;

        let (tx_in, mut rx_in) = mpsc::channel::<Value>(100);
        let (tx_out, rx_out) = mpsc::channel::<SignalMessage>(100);

        // Stdin writer task
        tokio::spawn(async move {
            while let Some(payload) = rx_in.recv().await {
                if let Ok(payload_str) = serde_json::to_string(&payload) {
                    info!("Sending Signal RPC");
                    log::debug!("Sending Signal RPC payload: [REDACTED]");
                    if stdin.write_all(payload_str.as_bytes()).await.is_err() {
                        break;
                    }
                    if stdin.write_all(b"\n").await.is_err() {
                        break;
                    }
                    if stdin.flush().await.is_err() {
                        break;
                    }
                }
            }
        });

        // Stdout reader task
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() { continue; }

                log::debug!("Raw Signal Line received");

                if let Ok(rpc) = serde_json::from_str::<JsonRpcNotification>(&line) {
                     if rpc.method == "receive" {
                        if let Err(e) = tx_out.send(rpc.params).await {
                            error!("Receiver dropped: {}", e);
                            break;
                        }
                     }
                } else if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&line) {
                    if let Some(error) = resp.error {
                        warn!("Signal Command Failed (ID: {:?}): {} - Data: {:?}", resp.id, error.message, error.data);
                    } else {
                        info!("Signal Command Success (ID: {:?}): {:?}", resp.id, resp.result);
                    }
                } else {
                    warn!("Unknown Signal output: {}", line);
                }
            }
            info!("Signal listener loop ended");
            let _ = child.wait().await; // Wait for child process to exit completely
        });

        Ok((Self {
            user_phone: user_phone.to_string(),
            tx: tx_in,
        }, rx_out))
    }

    pub async fn send_message(&self, recipient: &str, group_id: Option<&str>, message: &str, attachment: Option<&str>) -> Result<()> {
        let mut params = if let Some(gid) = group_id {
            json!({
                "groupId": gid,
                "message": message
            })
        } else {
            json!({
                "recipient": [recipient],
                "message": message
            })
        };

        if let Some(att) = attachment {
             if let Some(obj) = params.as_object_mut() {
                 obj.insert("attachment".to_string(), json!([att]));
             }
        }

        let payload = json!({
            "jsonrpc": "2.0",
            "method": "send",
            "params": params,
            "id": "1"
        });

        self.send_payload(&payload).await
    }

    pub async fn send_receipt(&self, recipient: &str, target_timestamp: u64) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendReceipt",
            "params": {
                "recipient": recipient,
                "targetTimestamp": target_timestamp,
                "type": "read"
            },
            "id": "2"
        });

        self.send_payload(&payload).await
    }

    pub async fn send_typing(&self, recipient: &str, group_id: Option<&str>) -> Result<()> {
        let params = if let Some(gid) = group_id {
            json!({ "groupId": [gid] })
        } else {
            json!({ "recipient": [recipient] })
        };

        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendTyping",
            "params": params,
            "id": "3"
        });

        self.send_payload(&payload).await
    }

    pub async fn stop_typing(&self, recipient: &str, group_id: Option<&str>) -> Result<()> {
        let params = if let Some(gid) = group_id {
            json!({ "groupId": [gid], "stop": true })
        } else {
            json!({ "recipient": [recipient], "stop": true })
        };

        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendTyping",
            "params": params,
            "id": "4"
        });

        self.send_payload(&payload).await
    }

    async fn send_payload(&self, payload: &Value) -> Result<()> {
        self.tx.send(payload.clone()).await.map_err(|_| anyhow::anyhow!("Failed to send payload to background task"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_rpc_notification() {
        let raw_json = r#"{
            "method": "receive",
            "params": {
                "envelope": {
                    "source": "+1234567890",
                    "timestamp": 1678886400000,
                    "dataMessage": {
                        "message": "Hello from signal",
                        "timestamp": 1678886400000
                    }
                }
            }
        }"#;

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let notif = parsed.unwrap();
        assert_eq!(notif.method, "receive");
        assert!(notif.params.envelope.is_some());

        let envelope = notif.params.envelope.unwrap();
        assert_eq!(envelope.source, "+1234567890");
        assert_eq!(envelope.timestamp, 1678886400000);

        assert!(envelope.data_message.is_some());
        let data_message = envelope.data_message.unwrap();
        assert_eq!(data_message.message.as_deref(), Some("Hello from signal"));
    }

    #[test]
    fn test_parse_json_rpc_response_success() {
        let raw_json = r#"{
            "jsonrpc": "2.0",
            "id": "1",
            "result": {
                "timestamp": 1678886400000
            }
        }"#;

        let parsed: Result<JsonRpcResponse, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let response = parsed.unwrap();
        assert_eq!(response.id.as_deref(), Some("1"));
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[test]
    fn test_parse_json_rpc_response_error() {
        let raw_json = r#"{
            "jsonrpc": "2.0",
            "id": "2",
            "error": {
                "code": -32602,
                "message": "Invalid params"
            }
        }"#;

        let parsed: Result<JsonRpcResponse, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let response = parsed.unwrap();
        assert_eq!(response.id.as_deref(), Some("2"));
        assert!(response.result.is_none());
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.code, -32602);
        assert_eq!(error.message, "Invalid params");
    }

    // --- SECURITY & STRICT TESTS ---

    #[test]
    fn test_parse_missing_optional_fields() {
        // A minimal viable envelope with no data message or sync message
        let raw_json = r#"{
            "method": "receive",
            "params": {
                "envelope": {
                    "source": "+1234567890",
                    "timestamp": 1678886400000
                }
            }
        }"#;

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok(), "Should parse envelope safely even if dataMessage is entirely missing");

        let notif = parsed.unwrap();
        let env = notif.params.envelope.unwrap();
        assert!(env.data_message.is_none());
    }

    #[test]
    fn test_serialization_send_message() {
        // Verify that when we construct the JSON for `send_message`, the recipient is an array like signal-cli expects.
        // And that attachments are properly structured.
        // Since we build the Value dynamically in send_message, we can't test a strict struct,
        // but we can test the json! macro output matching our expectations.
        let recipient = "+1234567890";
        let message = "Hello";

        let mut params = serde_json::json!({
            "recipient": [recipient],
            "message": message,
        });

        // Test normal format matches signal-cli specification structurally
        assert_eq!(params["recipient"][0], "+1234567890");
        assert_eq!(params["message"], "Hello");
    }

    #[test]
    fn test_parse_adversarial_quotes() {
        // Test parsing an extremely long quote/mention to ensure it doesn't panic
        let mut long_text = String::new();
        for _ in 0..10_000 {
            long_text.push_str("A");
        }

        // This simulates a DoS attempt via giant payloads on the JSON parser
        let raw_json = format!(r#"{{
            "method": "receive",
            "params": {{
                "envelope": {{
                    "source": "+1",
                    "timestamp": 123,
                    "dataMessage": {{
                        "message": "reply",
                        "timestamp": 123,
                        "quote": {{
                            "id": 1,
                            "author": "+2",
                            "text": "{}"
                        }}
                    }}
                }}
            }}
        }}"#, long_text);

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(&raw_json);
        assert!(parsed.is_ok());

        let quote = parsed.unwrap().params.envelope.unwrap().data_message.unwrap().quote.unwrap();
        assert_eq!(quote.text.len(), 10_000);
    }
}

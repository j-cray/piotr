use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
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

#[allow(dead_code)]
pub struct SignalClient {
    user_phone: String,
    child: Child,
    stdin: Option<ChildStdin>, // We might need to keep this to write to it
    // stdout reader will be moved to a background task
}

impl SignalClient {
    pub async fn new(user_phone: &str) -> Result<Self> {
        info!("Starting signal-cli for user: {}", user_phone);
        let mut child = Command::new("signal-cli")
            .arg("-u")
            .arg(user_phone)
            .arg("--output=json")
            .arg("jsonRpc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Log stderr to parent stderr
            .spawn()
            .context("Failed to spawn signal-cli")?;

        let stdin = child.stdin.take();

        Ok(Self {
            user_phone: user_phone.to_string(),
            child,
            stdin,
        })
    }

    pub async fn run_listener(&mut self) -> Result<mpsc::Receiver<SignalMessage>> {
        let stdout = self.child.stdout.take().context("No stdout handle")?;
        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() { continue; }

                // signal-cli jsonRpc output might behave differently than pure json output
                // But typically it sends events.
                // Let's try to parse as generic JSON first to see what we get, or directly to SignalMessage

                // Log raw line for debugging
                info!("Raw Signal JSON: {}", line);

                // Try parsing as Notification first
                if let Ok(rpc) = serde_json::from_str::<JsonRpcNotification>(&line) {
                     if rpc.method == "receive" {
                        if let Err(e) = tx.send(rpc.params).await {
                            error!("Receiver dropped: {}", e);
                            break;
                        }
                     }
                } else if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&line) {
                    // It's a response to a command (success or error)
                    if let Some(error) = resp.error {
                        warn!("Signal Command Failed (ID: {:?}): {} - Data: {:?}", resp.id, error.message, error.data);
                    } else {
                        // Success response, currently we don't correlate IDs but good to log at debug/info
                        info!("Signal Command Success (ID: {:?}): {:?}", resp.id, resp.result);
                    }
                } else {
                    // Unknown formation
                    warn!("Unknown Signal output: {}", line);
                }
            }
            info!("Signal listener loop ended");
        });

        Ok(rx)
    }

    pub async fn send_message(&mut self, recipient: &str, group_id: Option<&str>, message: &str, attachment: Option<&str>) -> Result<()> {
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

    pub async fn send_receipt(&mut self, recipient: &str, target_timestamp: u64) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendReceipt",
            "params": {
                "recipient": [recipient],
                "targetTimestamp": [target_timestamp],
                "type": "read"
            },
            "id": "2"
        });

        self.send_payload(&payload).await
    }

    pub async fn send_typing(&mut self, recipient: &str, group_id: Option<&str>) -> Result<()> {
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

    pub async fn stop_typing(&mut self, recipient: &str, group_id: Option<&str>) -> Result<()> {
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

    async fn send_payload(&mut self, payload: &Value) -> Result<()> {
        if let Some(stdin) = &mut self.stdin {
             let payload_str = serde_json::to_string(payload)?;
             info!("Sending Signal RPC: {}", payload_str);
             stdin.write_all(payload_str.as_bytes()).await?;
             stdin.write_all(b"\n").await?;
             stdin.flush().await?;
        } else {
            return Err(anyhow::anyhow!("Signal stdin is not available"));
        }
        Ok(())
    }
}

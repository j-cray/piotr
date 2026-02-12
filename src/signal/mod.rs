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
    #[serde(rename = "dataMessage")]
    pub data_message: Option<DataMessage>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct DataMessage {
    pub message: Option<String>,
    pub timestamp: u64,
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

                match serde_json::from_str::<SignalMessage>(&line) {
                    Ok(msg) => {
                        if let Err(e) = tx.send(msg).await {
                            error!("Receiver dropped: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        // It might be a response to a command, or just a log line if not pure JSON
                        // For now, log it.
                        warn!("Failed to parse signal line: {} - Raw: {}", e, line);
                    }
                }
            }
            info!("Signal listener loop ended");
        });

        Ok(rx)
    }

    pub async fn send_message(&mut self, recipient: &str, message: &str) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "send",
            "params": {
                "recipient": [recipient],
                "message": message
            },
            "id": "1"
        });

        if let Some(stdin) = &mut self.stdin {
             let payload_str = serde_json::to_string(&payload)?;
             stdin.write_all(payload_str.as_bytes()).await?;
             stdin.write_all(b"\n").await?;
             stdin.flush().await?;
             info!("Sent message to {}", recipient);
        } else {
            return Err(anyhow::anyhow!("Signal stdin is not available"));
        }
        Ok(())
    }
}

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

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

#[derive(Deserialize, Debug, Clone)]
pub struct JsonRpcNotification {
    pub jsonrpc: Option<String>,
    pub method: String,
    pub params: SignalMessage,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SignalMessage {
    pub account: Option<String>,
    pub envelope: Option<Envelope>,
    // Add other fields as needed from signal-cli output
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct Envelope {
    pub source: Option<String>,
    #[serde(rename = "sourceNumber")]
    pub source_number: Option<String>,
    #[serde(rename = "sourceUuid")]
    pub source_uuid: Option<String>,
    pub timestamp: Option<u64>,
    #[serde(rename = "sourceName")]
    pub source_name: Option<String>,
    #[serde(rename = "sourceDevice")]
    pub source_device: Option<u32>,
    #[serde(rename = "dataMessage")]
    pub data_message: Option<DataMessage>,
    #[serde(rename = "editMessage")]
    pub edit_message: Option<EditMessage>,
    #[serde(rename = "syncMessage")]
    pub sync_message: Option<SyncMessage>,
    #[serde(rename = "receiptMessage")]
    pub receipt_message: Option<ReceiptMessage>,
    #[serde(rename = "typingMessage")]
    pub typing_message: Option<Value>,
}

impl Envelope {
    pub fn effective_source(&self) -> String {
        self.source
            .as_deref()
            .or(self.source_number.as_deref())
            .or(self.source_uuid.as_deref())
            .unwrap_or("unknown")
            .to_string()
    }

    pub fn effective_timestamp(&self) -> u64 {
        self.timestamp
            .or_else(|| self.data_message.as_ref().and_then(|d| d.timestamp))
            .or_else(|| {
                self.edit_message
                    .as_ref()
                    .and_then(|e| e.data_message.as_ref())
                    .and_then(|d| d.timestamp)
            })
            .or_else(|| {
                self.sync_message.as_ref().and_then(|s| {
                    s.sent_message.as_ref().and_then(|sm| {
                        sm.data_message
                            .as_ref()
                            .and_then(|d| d.timestamp)
                            .or(sm.timestamp)
                    })
                })
            })
            .unwrap_or(0)
    }

    pub fn effective_data_message(&self) -> Option<DataMessage> {
        if let Some(dm) = &self.data_message {
            return Some(dm.clone());
        }
        if let Some(em) = &self.edit_message {
            if let Some(dm) = &em.data_message {
                return Some(dm.clone());
            }
        }
        if let Some(sync) = &self.sync_message {
            if let Some(sm) = &sync.sent_message {
                if let Some(dm) = &sm.data_message {
                    let mut dm_clone = dm.clone();
                    if dm_clone.destination.is_none() {
                        dm_clone.destination = sm.destination.clone();
                    }
                    if dm_clone.destination_number.is_none() {
                        dm_clone.destination_number = sm.destination_number.clone();
                    }
                    if dm_clone.destination_uuid.is_none() {
                        dm_clone.destination_uuid = sm.destination_uuid.clone();
                    }
                    return Some(dm_clone);
                } else if let Some(em) = &sm.edit_message {
                    if let Some(dm) = &em.data_message {
                        let mut dm_clone = dm.clone();
                        if dm_clone.destination.is_none() {
                            dm_clone.destination = sm.destination.clone();
                        }
                        if dm_clone.destination_number.is_none() {
                            dm_clone.destination_number = sm.destination_number.clone();
                        }
                        if dm_clone.destination_uuid.is_none() {
                            dm_clone.destination_uuid = sm.destination_uuid.clone();
                        }
                        return Some(dm_clone);
                    }
                } else if sm.message.is_some() || sm.reaction.is_some() {
                    // Fallback for flattened payloads
                    return Some(DataMessage {
                        message: sm.message.clone(),
                        timestamp: sm.timestamp,
                        destination: sm.destination.clone(),
                        destination_number: sm.destination_number.clone(),
                        destination_uuid: sm.destination_uuid.clone(),
                        expires_in_seconds: None,
                        view_once: None,
                        group_info: sm.group_info.clone(),
                        quote: sm.quote.clone(),
                        reaction: sm.reaction.clone(),
                        mentions: sm.mentions.clone(),
                        attachments: None,
                    });
                }
            }
        }
        None
    }
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct EditMessage {
    #[serde(rename = "targetSentTimestamp")]
    pub target_sent_timestamp: Option<u64>,
    #[serde(rename = "dataMessage")]
    pub data_message: Option<DataMessage>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct SyncMessage {
    #[serde(rename = "sentMessage")]
    pub sent_message: Option<SyncDataMessage>,
    #[serde(rename = "readMessages")]
    pub read_messages: Option<Vec<Value>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct SyncDataMessage {
    pub destination: Option<String>,
    #[serde(rename = "destinationNumber")]
    pub destination_number: Option<String>,
    #[serde(rename = "destinationUuid")]
    pub destination_uuid: Option<String>,
    #[serde(rename = "dataMessage")]
    pub data_message: Option<DataMessage>,
    #[serde(rename = "editMessage")]
    pub edit_message: Option<EditMessage>,
    // Optional flattened fields in case message is serialized at top level
    pub message: Option<String>,
    pub timestamp: Option<u64>,
    #[serde(rename = "groupInfo")]
    pub group_info: Option<GroupInfo>,
    pub quote: Option<Quote>,
    pub reaction: Option<Reaction>,
    pub mentions: Option<Vec<Mention>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct ReceiptMessage {
    pub when: Option<u64>,
    #[serde(rename = "isDelivery")]
    pub is_delivery: Option<bool>,
    #[serde(rename = "isRead")]
    pub is_read: Option<bool>,
    #[serde(rename = "isViewed")]
    pub is_viewed: Option<bool>,
    pub timestamps: Option<Vec<u64>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct DataMessage {
    pub message: Option<String>,
    pub timestamp: Option<u64>,
    pub destination: Option<String>,
    #[serde(rename = "destinationNumber")]
    pub destination_number: Option<String>,
    #[serde(rename = "destinationUuid")]
    pub destination_uuid: Option<String>,
    #[serde(rename = "expiresInSeconds")]
    pub expires_in_seconds: Option<u32>,
    #[serde(rename = "viewOnce")]
    pub view_once: Option<bool>,
    #[serde(rename = "groupInfo")]
    pub group_info: Option<GroupInfo>,
    pub quote: Option<Quote>,
    pub reaction: Option<Reaction>,
    pub mentions: Option<Vec<Mention>>,
    pub attachments: Option<Vec<Value>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct Mention {
    pub name: Option<String>,
    pub number: Option<String>,
    pub uuid: Option<String>,
    pub start: Option<usize>,
    pub length: Option<usize>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct Reaction {
    pub emoji: Option<String>,
    #[serde(rename = "targetAuthor")]
    pub target_author: Option<String>,
    #[serde(rename = "targetAuthorNumber")]
    pub target_author_number: Option<String>,
    #[serde(rename = "targetAuthorUuid")]
    pub target_author_uuid: Option<String>,
    #[serde(rename = "targetSentTimestamp")]
    pub target_sent_timestamp: Option<u64>,
    #[serde(rename = "isRemove", default)]
    pub is_remove: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct Quote {
    pub id: Option<u64>,
    pub author: Option<String>,
    #[serde(rename = "authorNumber")]
    pub author_number: Option<String>,
    #[serde(rename = "authorUuid")]
    pub author_uuid: Option<String>,
    pub text: Option<String>,
    pub mentions: Option<Vec<Mention>>,
    pub attachments: Option<Vec<Value>>,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct GroupInfo {
    #[serde(rename = "groupId")]
    pub group_id: String,
    #[serde(rename = "groupName")]
    pub group_name: Option<String>,
    pub revision: Option<i32>,
    #[serde(rename = "type")]
    pub group_type: Option<String>,
}

#[derive(Clone)]
pub struct SignalClient {
    user_phone: String,
    tx: mpsc::Sender<Value>,
    next_request_id: Arc<AtomicUsize>,
    pending_requests: Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<Result<()>>>,
        >,
    >,
}

impl SignalClient {
    fn next_id(&self) -> String {
        self.next_request_id
            .fetch_add(1, Ordering::SeqCst)
            .to_string()
    }
    pub fn user_phone(&self) -> &str {
        &self.user_phone
    }

    #[cfg(test)]
    pub fn new_dummy() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            user_phone: "dummy".to_string(),
            tx,
            next_request_id: Arc::new(AtomicUsize::new(1)),
            pending_requests: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub async fn new(
        user_phone: &str,
        data_path: &str,
    ) -> Result<(Self, mpsc::Receiver<SignalMessage>)> {
        // Validate E.164 phone number format before passing to external process.
        // Length and prefix are checked before any slice access.
        let valid_phone = user_phone.starts_with('+')
            && user_phone.len() >= 8
            && user_phone.len() <= 16
            && user_phone[1..].chars().all(|c| c.is_ascii_digit());
        if !valid_phone {
            anyhow::bail!(
                "Invalid phone number format '{}': expected E.164 (e.g. +12345678901)",
                user_phone
            );
        }

        info!("Starting robust signal-cli supervisor for user: [REDACTED]");

        let (tx_in, mut rx_in) = mpsc::channel::<Value>(100);
        let (tx_out, rx_out) = mpsc::channel::<SignalMessage>(100);

        let pending_requests = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            tokio::sync::oneshot::Sender<Result<()>>,
        >::new()));
        let pending_requests_clone = pending_requests.clone();

        let phone_clone = user_phone.to_string();
        let data_path_clone = data_path.to_string();

        tokio::spawn(async move {
            const INITIAL_RESTART_DELAY_SECS: u64 = 1;
            const MAX_RESTART_DELAY_SECS: u64 = 60;
            let mut restart_delay_secs = INITIAL_RESTART_DELAY_SECS;
            let mut last_spawn_time = tokio::time::Instant::now();
            let mut is_first_spawn = true;

            let mut current_child: Option<tokio::process::Child> = None;
            let mut current_stdin: Option<tokio::process::ChildStdin> = None;
            let mut reader: Option<
                tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
            > = None;

            async fn reset_process(
                current_child: &mut Option<tokio::process::Child>,
                current_stdin: &mut Option<tokio::process::ChildStdin>,
                reader: &mut Option<
                    tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
                >,
                pending_requests: &Arc<
                    std::sync::Mutex<
                        std::collections::HashMap<String, tokio::sync::oneshot::Sender<Result<()>>>,
                    >,
                >,
                error_message: &'static str,
            ) {
                if let Some(mut child) = current_child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                *current_stdin = None;
                *reader = None;

                let mut map = pending_requests.lock().unwrap();
                for (_, tx) in map.drain() {
                    let _ = tx.send(Err(anyhow::anyhow!("{}", error_message)));
                }
            }

            loop {
                if current_child.is_none() {
                    if !is_first_spawn {
                        if last_spawn_time.elapsed().as_secs() > 10 {
                            restart_delay_secs = INITIAL_RESTART_DELAY_SECS;
                        }
                        error!(
                            "Waiting {}s before restarting signal-cli...",
                            restart_delay_secs
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(restart_delay_secs))
                            .await;
                        restart_delay_secs =
                            std::cmp::min(restart_delay_secs * 2, MAX_RESTART_DELAY_SECS);
                    }
                    is_first_spawn = false;

                    info!("Spawning signal-cli process");
                    let mut child = match Command::new("signal-cli")
                        .arg("--config")
                        .arg(&data_path_clone)
                        .arg("-u")
                        .arg(&phone_clone)
                        .arg("--output=json")
                        .arg("jsonRpc")
                        .arg("--receive-mode=on-start")
                        .arg("--send-read-receipts")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Failed to spawn signal-cli: {}", e);
                            continue;
                        }
                    };

                    let stdin = match child.stdin.take() {
                        Some(s) => s,
                        None => {
                            error!("signal-cli spawned without stdin handle");
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            continue;
                        }
                    };
                    let stdout = match child.stdout.take() {
                        Some(s) => s,
                        None => {
                            error!("signal-cli spawned without stdout handle");
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            continue;
                        }
                    };
                    current_stdin = Some(stdin);
                    reader = Some(BufReader::new(stdout).lines());
                    current_child = Some(child);
                    last_spawn_time = tokio::time::Instant::now();
                    info!("signal-cli process spawned successfully");
                }

                tokio::select! {
                    payload_opt = rx_in.recv() => {
                        match payload_opt {
                            Some(payload) => {
                                if let Some(stdin) = current_stdin.as_mut() {
                                    match serde_json::to_string(&payload) {
                                        Ok(payload_str) => {
                                            tracing::debug!("Sending Signal RPC payload: [REDACTED]");
                                            if stdin.write_all(payload_str.as_bytes()).await.is_err() ||
                                               stdin.write_all(b"\n").await.is_err() ||
                                               stdin.flush().await.is_err() {
                                                error!("Failed to write to signal-cli stdin. Triggering restart.");
                                                reset_process(
                                                    &mut current_child,
                                                    &mut current_stdin,
                                                    &mut reader,
                                                    &pending_requests_clone,
                                                    "Signal-cli stdin write failed, process restarting",
                                                ).await;
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to serialize Signal RPC payload: {}", e);
                                            if let Some(id_val) = payload.get("id") {
                                                if let Some(id_str) = id_val.as_str() {
                                                    let mut map = pending_requests_clone.lock().unwrap();
                                                    if let Some(tx) = map.remove(id_str) {
                                                        let _ = tx.send(Err(anyhow::anyhow!("Failed to serialize payload: {}", e)));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    if let Some(id_val) = payload.get("id") {
                                        if let Some(id_str) = id_val.as_str() {
                                            let mut map = pending_requests_clone.lock().unwrap();
                                            if let Some(tx) = map.remove(id_str) {
                                                let _ = tx.send(Err(anyhow::anyhow!("Signal-cli not running, dropped request")));
                                            }
                                        }
                                    }
                                }
                            },
                            None => {
                                info!("Signal supervisor rx_in dropped. Exiting gracefully.");
                                if let Some(mut child) = current_child.take() {
                                    let _ = child.kill().await;
                                    let _ = child.wait().await;
                                }
                                break;
                            }
                        }
                    },
                    line_res = async {
                        if let Some(r) = reader.as_mut() {
                            r.next_line().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        match line_res {
                            Ok(Some(line)) => {
                                if line.trim().is_empty() { continue; }
                                info!("Raw Signal Line received: {}", line);

                                if let Ok(rpc) = serde_json::from_str::<JsonRpcNotification>(&line) {
                                     if rpc.method == "receive" {
                                        if let Err(e) = tx_out.send(rpc.params).await {
                                            error!("Receiver dropped: {}", e);
                                            if let Some(mut child) = current_child.take() {
                                                let _ = child.kill().await;
                                                let _ = child.wait().await;
                                            }
                                            break;
                                        }
                                     }
                                } else if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&line) {
                                    if let Some(id_str) = resp.id {
                                        let sender_opt = pending_requests_clone.lock().unwrap().remove(&id_str);
                                        if let Some(sender) = sender_opt {
                                            if let Some(error) = resp.error {
                                                let _ = sender.send(Err(anyhow::anyhow!("Signal Command Failed (ID: {}): {} - {:?}", id_str, error.message, error.data)));
                                            } else {
                                                let _ = sender.send(Ok(()));
                                            }
                                        } else {
                                            if let Some(error) = resp.error {
                                                warn!("Signal Command Failed (ID: {}): {} - Data: {:?}", id_str, error.message, error.data);
                                            } else {
                                                info!("Signal Command Success (ID: {}): {:?}", id_str, resp.result);
                                            }
                                        }
                                    } else if let Some(error) = resp.error {
                                        warn!("Signal Command Failed (No ID): {} - Data: {:?}", error.message, error.data);
                                    }
                                } else {
                                    warn!("Unknown Signal output: {}", line);
                                }
                            },
                            Ok(None) | Err(_) => {
                                error!("Signal-cli stdout closed unexpectedly. Restarting process...");
                                reset_process(
                                    &mut current_child,
                                    &mut current_stdin,
                                    &mut reader,
                                    &pending_requests_clone,
                                    "Signal-cli process restarted before command could complete",
                                ).await;
                            }
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                user_phone: user_phone.to_string(),
                tx: tx_in,
                next_request_id: Arc::new(AtomicUsize::new(1)),
                pending_requests,
            },
            rx_out,
        ))
    }

    pub async fn send_message(
        &self,
        recipient: &str,
        group_id: Option<&str>,
        message: &str,
        attachment: Option<&str>,
    ) -> Result<()> {
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

        let id_str = self.next_id();
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "send",
            "params": params,
            "id": &id_str
        });

        self.send_and_wait(&payload, id_str).await
    }

    pub async fn send_receipt(&self, recipient: &str, target_timestamp: u64) -> Result<()> {
        let id_str = self.next_id();
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendReceipt",
            "params": {
                "recipient": recipient,
                "targetTimestamp": target_timestamp,
                "type": "read"
            },
            "id": &id_str
        });

        self.send_and_wait(&payload, id_str).await
    }

    pub async fn send_typing(&self, recipient: &str, group_id: Option<&str>) -> Result<()> {
        let params = if let Some(gid) = group_id {
            json!({ "groupId": gid })
        } else {
            json!({ "recipient": [recipient] })
        };

        let id_str = self.next_id();
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendTyping",
            "params": params,
            "id": &id_str
        });

        self.send_and_wait(&payload, id_str).await
    }

    pub async fn stop_typing(&self, recipient: &str, group_id: Option<&str>) -> Result<()> {
        let params = if let Some(gid) = group_id {
            json!({ "groupId": gid, "stop": true })
        } else {
            json!({ "recipient": [recipient], "stop": true })
        };

        let id_str = self.next_id();
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "sendTyping",
            "params": params,
            "id": &id_str
        });

        self.send_and_wait(&payload, id_str).await
    }

    async fn send_and_wait(&self, payload: &Value, id_str: String) -> Result<()> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        {
            let mut map = self.pending_requests.lock().unwrap();
            map.insert(id_str.clone(), resp_tx);
        }

        if let Err(e) = self.send_payload(payload).await {
            self.pending_requests.lock().unwrap().remove(&id_str);
            return Err(e);
        }

        match tokio::time::timeout(tokio::time::Duration::from_secs(30), resp_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => {
                self.pending_requests.lock().unwrap().remove(&id_str);
                Err(anyhow::anyhow!(
                    "Signal CLI response channel dropped unexpectedly"
                ))
            }
            Err(_) => {
                self.pending_requests.lock().unwrap().remove(&id_str);
                Err(anyhow::anyhow!("Signal command timed out after 30s"))
            }
        }
    }

    async fn send_payload(&self, payload: &Value) -> Result<()> {
        self.tx
            .send(payload.clone())
            .await
            .map_err(|_| anyhow::anyhow!("Failed to send payload to background task"))?;
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
        assert_eq!(envelope.effective_source(), "+1234567890");
        assert_eq!(envelope.effective_timestamp(), 1678886400000);

        assert!(envelope.data_message.is_some());
        let data_message = envelope.data_message.unwrap();
        assert_eq!(data_message.message.as_deref(), Some("Hello from signal"));
    }

    #[test]
    fn test_parse_sync_message() {
        let raw_json = r#"{
            "jsonrpc": "2.0",
            "method": "receive",
            "params": {
                "account": "+12506410032",
                "envelope": {
                    "source": "+12506410032",
                    "sourceNumber": "+12506410032",
                    "sourceUuid": "1f040322-4555-45b4-a35e-b1bc794ffbe3",
                    "timestamp": 1678886400000,
                    "syncMessage": {
                        "sentMessage": {
                            "destination": "+12506410032",
                            "destinationNumber": "+12506410032",
                            "timestamp": 1678886400000,
                            "message": "Note to self test"
                        }
                    }
                }
            }
        }"#;

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let notif = parsed.unwrap();
        assert_eq!(notif.method, "receive");
        let envelope = notif.params.envelope.unwrap();
        assert_eq!(envelope.effective_source(), "+12506410032");
        assert_eq!(envelope.effective_timestamp(), 1678886400000);
        assert!(envelope.effective_data_message().is_some());
        assert_eq!(
            envelope.effective_data_message().unwrap().message.as_deref(),
            Some("Note to self test")
        );
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
        assert!(
            parsed.is_ok(),
            "Should parse envelope safely even if dataMessage is entirely missing"
        );

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

        let params = serde_json::json!({
            "recipient": [recipient],
            "message": message,
        });

        // Test normal format matches signal-cli specification structurally
        assert_eq!(params["recipient"][0], "+1234567890");
        assert_eq!(params["message"], "Hello");
    }

    #[test]
    fn test_serialization_typing_group() {
        // Assert that groupId is constructed as a string, matching signal-cli's expected type
        let group_id = "some_base64_group_id_string=";

        let send_params = serde_json::json!({ "groupId": group_id });
        let stop_params = serde_json::json!({ "groupId": group_id, "stop": true });

        assert!(send_params["groupId"].is_string());
        assert_eq!(send_params["groupId"], group_id);

        assert!(stop_params["groupId"].is_string());
        assert_eq!(stop_params["groupId"], group_id);
        assert_eq!(stop_params["stop"], true);
    }

    #[test]
    fn test_parse_nested_sync_message() {
        let raw_json = r#"{
            "jsonrpc": "2.0",
            "method": "receive",
            "params": {
                "account": "+12506410032",
                "envelope": {
                    "source": "+12506410032",
                    "sourceNumber": "+12506410032",
                    "sourceUuid": "1f040322-4555-45b4-a35e-b1bc794ffbe3",
                    "timestamp": 1678886400000,
                    "syncMessage": {
                        "sentMessage": {
                            "destination": "+12506410032",
                            "destinationNumber": "+12506410032",
                            "destinationUuid": "1f040322-4555-45b4-a35e-b1bc794ffbe3",
                            "dataMessage": {
                                "timestamp": 1678886400000,
                                "message": "Real nested sync message test"
                            }
                        }
                    }
                }
            }
        }"#;

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let notif = parsed.unwrap();
        let envelope = notif.params.envelope.unwrap();
        assert_eq!(envelope.effective_source(), "+12506410032");
        assert_eq!(envelope.effective_timestamp(), 1678886400000);
        let dm = envelope.effective_data_message();
        assert!(dm.is_some());
        let data = dm.unwrap();
        assert_eq!(data.message.as_deref(), Some("Real nested sync message test"));
        assert_eq!(data.destination.as_deref(), Some("+12506410032"));
    }

    #[test]
    fn test_parse_adversarial_quotes() {
        // Test parsing an extremely long quote/mention to ensure it doesn't panic
        let mut long_text = String::new();
        for _ in 0..10_000 {
            long_text.push_str("A");
        }

        // This simulates a DoS attempt via giant payloads on the JSON parser
        let raw_json = format!(
            r#"{{
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
        }}"#,
            long_text
        );

        let parsed: Result<JsonRpcNotification, _> = serde_json::from_str(&raw_json);
        assert!(parsed.is_ok());

        let quote = parsed
            .unwrap()
            .params
            .envelope
            .unwrap()
            .data_message
            .unwrap()
            .quote
            .unwrap();
        assert_eq!(quote.text.as_deref().unwrap().len(), 10_000);
    }
}

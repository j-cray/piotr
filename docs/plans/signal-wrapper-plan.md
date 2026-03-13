# Signal-CLI Wrapper Implementation Plan

## Overview
We will implement a `SignalClient` struct in `src/signal/mod.rs` that spawns `signal-cli` as a child process in JSON-RPC daemon mode.

## Proposed Changes
### `src/signal/mod.rs`
- **Dependency:** Use `tokio::process::Command` to spawn `signal-cli`.
- **Communication:** Use stdin/stdout for JSON-RPC.
- **Struct:** `SignalClient` holding the Child process handle and channels for communication.
- **Methods:**
    - `new(user_phone_number: &str)`: Spawns the process `signal-cli --output=json -u <phone> jsonRpc`.
    - `send_message(recipient: &str, message: &str)`: Sends a `send` JSON-RPC command.
    - `listen()`: Async stream of received messages (Envelope/SyncMessage).

## JSON-RPC Protocol
Signal-CLI uses newline-delimited JSON.
Request:
```json
{"jsonrpc":"2.0","method":"send","params":{"recipient":["+1234567890"],"message":"Hello World"},"id":"1"}
```

## Verification Plan
### Automated
- Compile check `cargo check`.
- Unit test for JSON serialization/deserialization of RPC structs.

### Manual
- Run the bot with a registered phone number (User needs to register/link).
- Verify successful startup of `signal-cli` process.
- Since we don't have a linked account in this environment, verification will be limited to:
    - Checking if `signal-cli` starts and reports "No account linked" or similar error, which confirms the wrapper works.
    - User needs to perform linking manually via `signal-cli link -n <device_name>` inside the `nix develop` shell.

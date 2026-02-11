# Signal Bot with Vertex AI Backend - Implementation Plan

## 1. Overview
This project aims to build a Rust-based Signal Messenger bot that leverages Google Cloud Vertex AI (Gemini models) to process messages and generate intelligent responses. The bot will manage conversation context, interface with Signal via a secure bridge, and use GCP services for AI inference.
**GCP Project ID:** `piotr-487123`

## 2. Phases

### Phase 1: Foundation & Environment Setup
**Goal:** Establish a robust, reproducible development environment.
- **Milestone 1.1:** Setup Nix flake with Rust toolchain and dependencies.
- **Milestone 1.2:** Configure direnv for automatic environment loading.
- **Milestone 1.3:** Create Google Cloud Project and Service Account.
- **Milestone 1.4:** Initialize Rust project structure.

### Phase 2: Signal Interface Implementation
**Goal:** Enable secure message sending and receiving.
- **Milestone 2.1:** Configure `signal-cli` in Nix environment.
    - *Decision Point:* Switched to `signal-cli` (daemon mode) via JSON-RPC as the Rust crate strategy was ambiguous.
- **Milestone 2.2:** Implement `SignalClient` wrapper in Rust (spawning `signal-cli` process).
- **Milestone 2.3:** Create a robust message listener loop.
- **Milestone 2.4:** Implement message sending capability.

### Phase 3: Vertex AI Integration
**Goal:** connect the bot to Google's generative AI models.
- **Milestone 3.1:** Implement GCP authentication flow using Service Account credentials.
- **Milestone 3.2:** Develop a `VertexClient` struct to interact with the Vertex AI Prediction API.
- **Milestone 3.3:** Create prompt templates and context management logic.
- **Milestone 3.4:** Implement error handling and rate limiting for API calls.

### Phase 4: Bot Logic & Orchestration
**Goal:** Combine Signal I/O with AI logic.
- **Milestone 4.1:** Implement a `SessionManager` to track user conversations.
- **Milestone 4.2:** Develop the main event loop: Receive -> Process -> Generate -> Reply.
- **Milestone 4.3:** Add command support (e.g., `/reset`, `/help`).

### Phase 5: Deployment & Hardening
**Goal:** Prepare for production-grade operation.
- **Milestone 5.1:** Dockerize the application for deployment.
- **Milestone 5.2:** Set up persistent storage for Signal data and session history.
- **Milestone 5.3:** Implement logging and monitoring.

## 3. Detailed Subtasks

### Phase 1: Environment
- [ ] Create `flake.nix` with `rust-bin`, `openssl`, `pkg-config`.
- [ ] Create `.envrc` and `.gitignore`.
- [ ] Initialize `cargo init`.

### Phase 2: Signal Layer
- [ ] Add `signal-cli` to `flake.nix`.
- [ ] Remove `presage` from `Cargo.toml`.
- [ ] Implement `SignalClient` struct to spawn/manage `signal-cli`.
- [ ] Implement `receive_messages` loop parsing JSON-RPC.

### Phase 3: AI Layer
- [ ] Add dependencies: `reqwest`, `serde`, `serde_json`, `gcp-auth` (or similar).
- [ ] Create `src/vertex/mod.rs`.
- [ ] Implement `generate_content` function for Gemini Pro.

### Phase 4: Core Logic
- [ ] Create `src/bot.rs` for central logic.
- [ ] Implement state management using `dashmap` or `redis` (optional).

# Piotr
A privacy-focused, self-hosted AI assistant for Signal, built with Gemini, Qdrant, and Signal-CLI.

## Features

- **Signal Integration**: Listen to and send messages directly via Signal-CLI.
- **Context-Aware AI**: Uses Gemini for intelligent responses and maintains conversation history.
- **Memory System**: Learns from past conversations and user interactions.
- **Profile Management**: Keeps track of individual user preferences and details.
- **Group Chat Support**: Fully functional in group chats with proper context handling.
- **Privacy-Preserving**: Option to anonymize message sources in logs and responses.
- **Resumable**: Built-in queue and recovery mechanisms for seamless operation.
- **Resource Efficient**: Configurable concurrency limits and request timeouts.

## Quick Start

### Prerequisites

- **Rust**: Version 1.75 or higher.
- **Signal-CLI**: Installed and registered with a Signal account.
- **Google Gemini API Key**: Required for AI features.
- **Qdrant Vector Database**: Running instance (can be started locally or used remotely).

### Installation

1. Clone the repository:
   ```bash
   git clone <repository-url>
   cd piotr
   ```

2. Install dependencies:
   ```bash
   cargo build
   ```

### Configuration

Create a `.env` file in the root directory (or copy `.env.example`):

```env
# Google Gemini API Configuration
GEMINI_API_KEY=your_gemini_api_key_here
GEMINI_MODEL_NAME=gemini-2.0-flash-001

# Signal Configuration
SIGNAL_PHONE_NUMBER=+1234567890
SIGNAL_DATA_PATH=/path/to/signal-cli/data

# Database Configuration
DATABASE_URL=postgresql://user:password@localhost:5432/piotr

# Security
ANONYMIZE_KEY=your_random_key_here
PROFILE_ENCRYPTION_KEY=your_other_random_key_here
```

### Running

Start the bot:
```bash
cargo run
```

## Architecture

```mermaid
graph TD
    Signal[Signal-CLI] -->|Messages| SM[SessionManager]
    SM -->|Query & Update| Memory[Memory System]
    SM -->|Manage| ProfileMgr[Profile Manager]
    SM -->|Generate Response| Gemini[Gemini Client]
    SM -->|Store State| DB[Qdrant Database]
    SM -->|Send Reply| Signal

    subgraph Bot Core
        SM
        Memory
        ProfileMgr
    end

    subgraph AI Services
        Gemini
        DB
    end
```

## Data Model

- **Profiles**: User-specific data (e.g., nicknames, preferences).
  - Stored encrypted in Qdrant.
  - Indexed by anonymized source ID.

- **Memory**: Conversation history and learned facts.
  - Includes both Qdrant vectors and RAG context.
  - Separated by context key (group ID or conversation partner).

## Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## License

ISC

#!/usr/bin/env sh
set -e

# Configuration
export PGHOST=localhost
DB_NAME="piotr"
CURRENT_USER=$(whoami)
DB_USER="$CURRENT_USER"

# Generate or Retrieve Password
SECURE_PASS=""
if [ -f .env ]; then
    # Try to extract password from DATABASE_URL
    # Format: postgres://USER:PASSWORD@HOST/DB
    # We use sed to capture the part between the second colon and the @ sign
    EXTRACTED_PASS=$(grep "DATABASE_URL" .env | sed -n 's/^DATABASE_URL=postgres:\/\/[^:]*:\([^@]*\)@.*/\1/p')
    if [ -n "$EXTRACTED_PASS" ]; then
        echo "Found existing password in .env."
        SECURE_PASS="$EXTRACTED_PASS"
    fi
    EXTRACTED_PORT=$(grep "DATABASE_URL" .env | sed -n 's/^DATABASE_URL=postgres:\/\/[^:]*:[^@]*@[^:]*:\([0-9]*\)\/.*/\1/p')
    if [ -n "$EXTRACTED_PORT" ]; then
        PGPORT="$EXTRACTED_PORT"
    fi
fi

PGPORT="${PGPORT:-5432}"

if [ -z "$SECURE_PASS" ]; then
    echo "Generating new secure password..."
    SECURE_PASS=$(openssl rand -hex 16)
fi

# 0. Ensure Database is Running
PGDATA="$(pwd)/data/db"
export PGDATA

if command -v initdb > /dev/null 2>&1; then
    if [ ! -d "$PGDATA" ]; then
        echo "Initializing PostgreSQL data directory at $PGDATA..."
        # Use the secure password for new init
        PWFILE=$(mktemp)
        echo "$SECURE_PASS" > "$PWFILE"
        initdb -D "$PGDATA" --auth=md5 --pwfile="$PWFILE" --username="$CURRENT_USER"
        rm -f "$PWFILE"
        export PGPASSWORD="$SECURE_PASS"
    else
        echo "Data directory $PGDATA exists."
    fi

    # Check if running
    if ! pg_ctl status -D "$PGDATA" > /dev/null 2>&1; then
        if command -v python3 >/dev/null 2>&1; then
            while ! python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1', $PGPORT))" 2>/dev/null; do
                PGPORT=$((PGPORT + 1))
            done
        elif command -v nc >/dev/null 2>&1; then
            while nc -z localhost "$PGPORT" >/dev/null 2>&1; do
                PGPORT=$((PGPORT + 1))
            done
        fi
        export PGPORT

        echo "Starting PostgreSQL on port $PGPORT..."
        pg_ctl start -D "$PGDATA" -l "$PGDATA/logfile" -o "-p $PGPORT -k '$PGDATA'"
        sleep 2
    else
        if [ -f "$PGDATA/postmaster.pid" ]; then
            RUNNING_PORT=$(awk 'NR==4' "$PGDATA/postmaster.pid")
            [ -n "$RUNNING_PORT" ] && PGPORT="$RUNNING_PORT"
        fi
        export PGPORT
        echo "PostgreSQL is already running on port $PGPORT."
    fi
else
    echo "Error: initdb not found. Please run this script inside 'nix develop'."
    exit 1
fi

# Rotate Password if needed
# Try connecting with the Secure Password
export PGPASSWORD="$SECURE_PASS"
if psql -U "$DB_USER" -d postgres -c "\q" > /dev/null 2>&1; then
    echo "Connection successful with secure password."
else
    # Try connecting with default "password"
    export PGPASSWORD="password"
    if psql -U "$DB_USER" -d postgres -c "\q" > /dev/null 2>&1; then
        echo "Default password detected. Rotating to secure password..."
        psql -U "$DB_USER" -d postgres -c "ALTER USER \"$DB_USER\" WITH PASSWORD '$SECURE_PASS';"
        export PGPASSWORD="$SECURE_PASS"
        echo "Password rotated successfully."
    else
        echo "Warning: Could not connect with secure or default password. Proceeding with existing .env configuration if possible."
        # Attempt to read from .env if present?
        # For now, we assume the rotation worked or we are in a manual state.
    fi
fi

# 1. Create Database if it doesn't exist
if psql -U "$DB_USER" -d postgres -lqt | cut -d \| -f 1 | grep -qw "$DB_NAME"; then
    echo "Database '$DB_NAME' already exists."
else
    echo "Creating database '$DB_NAME'..."
    createdb -U "$DB_USER" "$DB_NAME"
fi

# 2. Generate Encryption Key (32 bytes = 64 hex chars)
if grep -q "PROFILE_ENCRYPTION_KEY" .env; then
    echo "PROFILE_ENCRYPTION_KEY already set in .env."
else
    echo "Generating new encryption key..."
    KEY=$(openssl rand -hex 32)
    echo "" >> .env
    echo "# Database Encryption Key" >> .env
    echo "PROFILE_ENCRYPTION_KEY=$KEY" >> .env
    echo "Added PROFILE_ENCRYPTION_KEY to .env"
fi

# 3. Add or Update Database URL in .env
# We always update/overwrite the DATABASE_URL to ensure it has the correct current password
if grep -q "DATABASE_URL" .env; then
    # Use sed to replace the line
    # Escape slash for sed
    ESCAPED_URL="postgres://${DB_USER}:${SECURE_PASS}@localhost:${PGPORT}/${DB_NAME}"
    # This sed pattern is simple; assuming standard format.
    # Note: password might contain special chars? openssl hex does not.
    sed "s|^DATABASE_URL=.*|DATABASE_URL=$ESCAPED_URL|" .env > .env.tmp && mv .env.tmp .env
    echo "Updated DATABASE_URL in .env with new credentials."
else
    echo "Adding DATABASE_URL to .env..."
    echo "" >> .env
    echo "# Database Connection" >> .env
    echo "DATABASE_URL=postgres://${DB_USER}:${SECURE_PASS}@localhost:${PGPORT}/${DB_NAME}" >> .env
    echo "Added DATABASE_URL to .env"
fi

if ! grep -q "ANONYMIZE_KEY" .env; then
    echo "Generating new anonymization key..."
    echo "ANONYMIZE_KEY=$(openssl rand -hex 16)" >> .env
    echo "Added ANONYMIZE_KEY to .env"
fi

echo "Setup complete! Run 'cargo run --bin piotr' to start the application."

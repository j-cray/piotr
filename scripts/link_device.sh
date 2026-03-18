#!/usr/bin/env bash
set -euo pipefail

# This script runs signal-cli link, captures the tsdevice URI,
# generates a QR code, and keeps signal-cli running.

echo "Starting signal-cli linking process..."
echo "Please wait for the QR code to appear."

echo "Starting signal-cli REST API container to provide a dedicated dbus environment..."
trap "docker rm -f piotr-signal-linker >/dev/null 2>&1" EXIT
docker run -d --rm --name piotr-signal-linker -v "$(pwd)/data/signal-cli:/home/.local/share/signal-cli" bbernhard/signal-cli-rest-api:latest >/dev/null

echo "Waiting for container to initialize..."
sleep 3

echo "Requesting link..."
docker exec -it piotr-signal-linker signal-cli --config /home/.local/share/signal-cli link -n "piotr-bot"

EXIT_CODE=$?
if [ $EXIT_CODE -eq 0 ]; then
    echo "Device linked successfully!"
else
    echo "Linking failed or was interrupted with exit code $EXIT_CODE."
fi

echo "Cleaning up container..."
docker rm -f piotr-signal-linker >/dev/null

echo "Restoring file permissions..."
docker run --rm -v "$(pwd)/data/signal-cli:/data" alpine chown -R $(id -u):$(id -g) /data

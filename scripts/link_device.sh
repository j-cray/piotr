#!/usr/bin/env bash

# This script runs signal-cli link, captures the tsdevice URI,
# generates a QR code, and keeps signal-cli running.

echo "Starting signal-cli linking process..."
echo "Please wait for the QR code to appear."

# Create a minimal dbus session config
cat > dbus-session.conf <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:tmpdir=/tmp</listen>
  <standard_session_servicedirs />
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF

# Run signal-cli in the background, redirecting output to a file
# usage of dbus-run-session is required for signal-cli
dbus-run-session --config-file=./dbus-session.conf -- signal-cli link -n "piotr-bot" > link_output.log 2>&1 &
SIGNAL_PID=$!

# Wait for the URI to appear in the log file
URI=""
while [ -z "$URI" ]; do
    if ! kill -0 $SIGNAL_PID 2>/dev/null; then
        echo "signal-cli process died unexpectedly. Check link_output.log:"
        cat link_output.log
        exit 1
    fi
    # Search for tsdevice or sgnl protocol link
    # Using [^[:space:]]+ to capture until whitespace or end of line
    URI=$(grep -o -E "(tsdevice:|sgnl://)[^[:space:]]+" link_output.log | head -n 1)
    sleep 1
done

echo "URI found: $URI"
echo "If the QR code below fails, copy the above URI and convert it to a QR code manually."


# Generate QR code
# Using python script for better ASCII rendering
python3 scripts/generate_qr.py "$URI"

echo ""
echo "Scan the QR code above with your primary Signal device."
echo "Waiting for you to complete the linking process..."

# Wait for signal-cli to finish (it exits when linking is complete)
wait $SIGNAL_PID

EXIT_CODE=$?
if [ $EXIT_CODE -eq 0 ]; then
    echo "Device linked successfully!"
else
    echo "Linking failed with exit code $EXIT_CODE. Check link_output.log."
fi

# Cleanup - keeping log for debugging if needed
rm dbus-session.conf

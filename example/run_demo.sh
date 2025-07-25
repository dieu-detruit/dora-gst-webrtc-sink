#!/bin/bash

echo "=== dora-gst-webrtc-sink Demo ==="
echo

# Check if dora is installed
if ! command -v dora &> /dev/null; then
    echo "Error: 'dora' command not found. Please install dora-rs first."
    echo "Visit: https://github.com/dora-rs/dora"
    exit 1
fi

# Check if GStreamer WebRTC plugin is installed
if ! command -v gst-inspect-1.0 webrtc &> /dev/null; then
    echo "Error: GStreamer not found. Please install GStreamer and its WebRTC plugins."
    echo "Ubuntu/Debian: sudo apt-get install libgstreamer1.0-dev libgstreamer-plugins-bad1.0-dev"
    exit 1
fi

echo
echo "Building dataflow..."
dora build dataflow.yml
if [ $? -ne 0 ]; then
    echo "Error: Build failed. Please check the error messages above."
    exit 1
fi

echo
echo "Starting dora dataflow..."
echo "WebRTC signaling server will be available at: ws://localhost:8080/ws"
echo
echo "To view the stream:"
echo "1. Open 'webrtc-viewer.html' in a web browser"
echo "2. Click 'Connect' button"
echo
echo "Press Ctrl+C to stop"
echo

# Start the dataflow
dora run dataflow.yml

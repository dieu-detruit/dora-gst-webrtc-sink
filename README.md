# dora-gst-webrtc-sink

A dora-rs node that receives images and streams them via WebRTC with built-in signaling server.

## Features

- Receives RGB8 format images from dora-rs input
- Streams video via WebRTC to multiple clients
- Built-in WebRTC signaling server
- Latest frame priority (channel buffer size of 1)
- Multiple client support

## Environment Variables

- `SIGNALING_PORT`: WebSocket signaling server port (default: 8080)

## Usage

### Running the Node

```bash
# Set the signaling server port (optional)
export SIGNALING_PORT=8080

# Run as part of a dora dataflow
dora start dataflow.yml
```

### Input Format

The node expects images on the "image" input with the following metadata:
- `encoding`: "rgb8" (currently only RGB8 is supported)
- `width`: Image width (currently fixed at 640)
- `height`: Image height (currently fixed at 480)

### WebRTC Client Connection

1. Connect to the WebSocket signaling server at `ws://localhost:8080/ws`
2. Send an offer SDP:
   ```json
   {
     "type": "offer",
     "sdp": "...",
     "id": "client-id"
   }
   ```
3. Receive answer SDP and ICE candidates
4. Start receiving the video stream

## Example Dataflow Configuration

```yaml
nodes:
  - id: camera
    path: camera-node
    outputs:
      - image
  
  - id: webrtc-sink
    path: dora-gst-webrtc-sink
    inputs:
      - image: camera/image
```

## Dependencies

- GStreamer with WebRTC support
- VP8 video codec
- STUN server (uses Google's public STUN server by default)

## Notes

- The video is encoded as VP8 at 640x480 resolution, 30fps
- Uses lossy encoding with deadline=1 for low latency
- Requires GStreamer 1.16+ with webrtcbin element
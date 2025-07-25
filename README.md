# dora-gst-webrtc-sink

A dora-rs node that receives images and streams them via WebRTC with built-in signaling server. Supports multiple video sources and multiple clients per source.

## Features

- Receives RGB8 format images from dora-rs input
- Streams video via WebRTC to multiple clients
- Built-in WebRTC signaling server
- Latest frame priority (channel buffer size of 1)
- **Multiple video sources**: Handle multiple cameras/video streams simultaneously
- **Multiple clients per source**: Each video source can serve multiple WebRTC clients
- Dynamic video source management

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

The node supports two input formats:

1. **Legacy format** (backward compatible):
   - Input ID: `image`
   - This maps to the default video source (`default`)

2. **Multi-camera format**:
   - Input ID: `<video_id>/frame`
   - Example: `camera1/frame`, `camera2/frame`, `front_camera/frame`
   - Each unique `video_id` creates a separate video stream

All inputs expect the following metadata:
- `encoding`: "rgb8" (currently only RGB8 is supported)
- `width`: Image width (currently fixed at 640)
- `height`: Image height (currently fixed at 480)

### WebRTC Client Connection

1. Connect to the WebSocket signaling server at `ws://localhost:8080/<video_id>`
   - Example: `ws://localhost:8080/camera1`
   - For legacy support: `ws://localhost:8080/default`
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

### Multi-Camera HTML Client

Use the provided `example/webrtc-viewer-multi.html` to connect to multiple video sources simultaneously. The client supports:
- Dynamic addition/removal of video streams
- Individual connection control per stream
- Real-time status and logging per stream

## Example Dataflow Configurations

### Single Camera (Legacy)
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

### Multiple Cameras
```yaml
nodes:
  - id: front-camera
    path: camera-node
    outputs:
      - camera1/frame
  
  - id: rear-camera
    path: camera-node
    outputs:
      - camera2/frame
  
  - id: webrtc-sink
    path: dora-gst-webrtc-sink
    inputs:
      - source: front-camera/camera1/frame
        queue_size: 1
      - source: rear-camera/camera2/frame
        queue_size: 1
```

## Example Scripts

### Single Camera Demo
```bash
cd example
./run_demo.sh
# Open webrtc-viewer.html in your browser
```

### Multi-Camera Demo
```bash
cd example
./run_demo_multi.sh
# Open webrtc-viewer-multi.html in your browser
```

## Dependencies

- GStreamer with WebRTC support
- VP8 video codec
- STUN server (uses Google's public STUN server by default)

## Notes

- The video is encoded as VP8 at 640x480 resolution, 30fps
- Uses lossy encoding with deadline=1 for low latency
- Requires GStreamer 1.16+ with webrtcbin element
- Each video source maintains its own pipeline and can serve multiple clients
- Video sources are created on-demand when the first client connects
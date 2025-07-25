use anyhow::Context;
use dora_node_api::{DoraNode, Event};
use futures::stream::StreamExt;
use futures::SinkExt;
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;
use gstreamer_webrtc::{WebRTCICEConnectionState, WebRTCSessionDescription};
use glib;
use arrow::array::UInt8Array;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;
use warp::ws::{Message, WebSocket};
use warp::Filter;

#[derive(Debug, Clone)]
struct PeerConnection {
    _id: String,
    pipeline: gstreamer::Pipeline,
    webrtcbin: gstreamer::Element,
    appsrc: AppSrc,
}

#[derive(Debug, Clone)]
struct WebRTCServer {
    peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    frame_sender: mpsc::Sender<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum SignalingMessage {
    #[serde(rename = "offer")]
    Offer { sdp: String, id: String },
    #[serde(rename = "answer")]
    Answer { sdp: String, id: String },
    #[serde(rename = "ice")]
    Ice {
        candidate: String,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: u32,
        id: String,
    },
}

impl WebRTCServer {
    fn new(frame_sender: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            frame_sender,
        }
    }

    async fn handle_websocket(&self, ws: WebSocket, id: String) {
        let (ws_sender, mut ws_receiver) = ws.split();
        let ws_sender = Arc::new(Mutex::new(ws_sender));

        while let Some(result) = ws_receiver.next().await {
            match result {
                Ok(msg) => {
                    if let Ok(text) = msg.to_str() {
                        if let Err(e) = self.handle_signaling_message(text, &id, ws_sender.clone()).await {
                            error!("Error handling signaling message: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
            }
        }

        self.remove_peer(&id);
        info!("Client {} disconnected", id);
    }

    async fn handle_signaling_message(
        &self,
        msg: &str,
        peer_id: &str,
        ws_sender: Arc<Mutex<futures::stream::SplitSink<WebSocket, Message>>>,
    ) -> anyhow::Result<()> {
        let message: SignalingMessage = serde_json::from_str(msg)?;

        match message {
            SignalingMessage::Offer { sdp, .. } => {
                info!("Received offer from {}", peer_id);
                let _answer_sdp = self.handle_offer(&sdp, peer_id, ws_sender).await?;
                debug!("Sending answer to {}", peer_id);
            }
            SignalingMessage::Ice {
                candidate,
                sdp_mline_index,
                ..
            } => {
                self.add_ice_candidate(peer_id, &candidate, sdp_mline_index)?;
            }
            _ => {
                warn!("Unexpected message type");
            }
        }

        Ok(())
    }

    async fn handle_offer(
        &self,
        sdp: &str,
        peer_id: &str,
        ws_sender: Arc<Mutex<futures::stream::SplitSink<WebSocket, Message>>>,
    ) -> anyhow::Result<String> {
        let pipeline = self.create_pipeline(peer_id, ws_sender.clone())?;
        
        let webrtcbin = pipeline
            .by_name("webrtc")
            .context("Failed to get webrtcbin")?;

        let appsrc = pipeline
            .by_name("videosrc")
            .and_then(|e| e.dynamic_cast::<AppSrc>().ok())
            .context("Failed to get appsrc")?;
        
        // Configure appsrc
        appsrc.set_property("is-live", true);
        appsrc.set_property("format", gstreamer::Format::Time);

        // Set pipeline to READY state before setting remote description
        debug!("Setting pipeline to READY state");
        pipeline.set_state(gstreamer::State::Ready)?;
        
        // Wait for state change to complete
        let (state_change_return, _, _) = pipeline.state(gstreamer::ClockTime::from_seconds(5));
        if state_change_return.is_err() {
            return Err(anyhow::anyhow!("Failed to set pipeline to READY state"));
        }

        let ret = gstreamer_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|_| anyhow::anyhow!("Failed to parse SDP"))?;
        
        debug!("Parsed SDP: {} media sections", ret.medias_len());
        
        let offer = WebRTCSessionDescription::new(
            gstreamer_webrtc::WebRTCSDPType::Offer,
            ret,
        );

        debug!("Setting remote description");
        let promise = gstreamer::Promise::new();
        webrtcbin.emit_by_name::<()>("set-remote-description", &[&offer, &promise]);
        
        // Wait for set-remote-description to complete
        let result = promise.wait();
        match result {
            gstreamer::PromiseResult::Replied => {
                // Check if there was an error in the reply
                if let Some(reply) = promise.get_reply() {
                    if let Ok(error_value) = reply.value("error") {
                        if let Ok(error) = error_value.get::<glib::Error>() {
                            error!("set-remote-description error: {}", error);
                            return Err(anyhow::anyhow!("Failed to set remote description: {}", error));
                        }
                    }
                }
                
                debug!("Remote description promise replied successfully");
                
                // Check if remote description is actually set
                if let Some(remote_desc) = webrtcbin.property::<Option<gstreamer_webrtc::WebRTCSessionDescription>>("remote-description") {
                    debug!("Remote description is set, SDP length: {}", remote_desc.sdp().to_string().len());
                } else {
                    error!("Remote description property is None after setting!");
                    
                    // Try different property name
                    let signaling_state = webrtcbin.property::<gstreamer_webrtc::WebRTCSignalingState>("signaling-state");
                    error!("Current signaling state: {:?}", signaling_state);
                }
            }
            _ => {
                error!("Failed to set remote description: {:?}", result);
                return Err(anyhow::anyhow!("Failed to set remote description"));
            }
        }
        
        // Additional delay to ensure state is propagated
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Store peer first to ensure it can receive frames
        let peer = PeerConnection {
            _id: peer_id.to_string(),
            pipeline: pipeline.clone(),
            webrtcbin: webrtcbin.clone(),
            appsrc: appsrc.clone(),
        };
        self.peers.lock().unwrap().insert(peer_id.to_string(), peer.clone());
        
        // Now set pipeline to PLAYING state before creating answer
        debug!("Setting pipeline to PLAYING state before creating answer");
        pipeline.set_state(gstreamer::State::Playing)?;
        
        // Wait for state change to complete
        let (state_change_return, _, _) = pipeline.state(gstreamer::ClockTime::from_seconds(5));
        if state_change_return.is_err() {
            warn!("Pipeline state change to PLAYING not fully successful: {:?}", state_change_return);
        }
        
        // Give pipeline time to initialize
        std::thread::sleep(std::time::Duration::from_millis(200));
        
        // Send a test frame to ensure the pipeline is ready
        let test_frame = vec![0u8; 640 * 480 * 3];
        self.send_frame_to_peers(&test_frame);

        // Connect to pad-added signal to ensure pipeline is ready
        let peer_id_for_pad = peer_id.to_string();
        webrtcbin.connect("pad-added", false, move |values| {
            let _webrtc = match values[0].get::<gstreamer::Element>() {
                Ok(elem) => elem,
                Err(_) => {
                    error!("Failed to get webrtc element from values");
                    return None;
                }
            };
            
            let pad = match values[1].get::<gstreamer::Pad>() {
                Ok(p) => p,
                Err(_) => {
                    error!("Failed to get pad from values");
                    return None;
                }
            };
            
            debug!("Pad added for {}: {}", peer_id_for_pad, pad.name());
            None
        });

        // Check signaling state before creating answer
        let signaling_state = webrtcbin.property::<gstreamer_webrtc::WebRTCSignalingState>("signaling-state");
        debug!("Signaling state before create-answer: {:?}", signaling_state);
        
        // Use promise without change func first
        let promise = gstreamer::Promise::new();
        
        debug!("Creating answer");
        webrtcbin.emit_by_name::<()>("create-answer", &[&None::<gstreamer::Structure>, &promise]);
        
        // Wait for promise to complete
        let reply = promise.wait();
        debug!("Promise completed: {:?}", reply);
        
        // Small delay to ensure the answer is fully processed
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // Get the answer from promise reply
        match reply {
            gstreamer::PromiseResult::Replied => {
                if let Some(reply_struct) = promise.get_reply() {
                    debug!("Got reply structure with fields: {:?}", reply_struct.fields().collect::<Vec<_>>());
                    
                    // Debug: print all fields and their types
                    for field in reply_struct.fields() {
                        debug!("Field '{}' type: {:?}", field, reply_struct.value(field).map(|v| v.type_()));
                        
                        // If it's an error, print the error message
                        if field == "error" {
                            if let Ok(error_value) = reply_struct.value("error") {
                                if let Ok(error) = error_value.get::<glib::Error>() {
                                    error!("create-answer error: {}", error);
                                }
                            }
                        }
                    }
                    
                    // The answer should be in the reply
                    if let Ok(answer_value) = reply_struct.get::<gstreamer_webrtc::WebRTCSessionDescription>("answer") {
                        debug!("Got answer from reply");
                        
                        // Set local description
                        let set_promise = gstreamer::Promise::new();
                        webrtcbin.emit_by_name::<()>("set-local-description", &[&answer_value, &set_promise]);
                        let _ = set_promise.wait();
                        
                        let answer_sdp = answer_value.sdp().to_string();
                        info!("Answer SDP created, length: {}", answer_sdp.len());
                        
                        let msg = SignalingMessage::Answer {
                            sdp: answer_sdp,
                            id: peer_id.to_string(),
                        };

                        if let Ok(json) = serde_json::to_string(&msg) {
                            info!("Sending answer JSON to client: {} chars", json.len());
                            let ws_sender_clone = ws_sender.clone();
                            tokio::task::spawn_blocking(move || {
                                let rt = tokio::runtime::Handle::current();
                                rt.block_on(async move {
                                    if let Ok(mut sender) = ws_sender_clone.lock() {
                                        match sender.send(Message::text(json)).await {
                                            Ok(_) => info!("Answer sent to client successfully"),
                                            Err(e) => error!("Failed to send answer: {:?}", e),
                                        }
                                    } else {
                                        error!("Failed to lock WebSocket sender");
                                    }
                                });
                            });
                        } else {
                            error!("Failed to serialize answer message");
                        }
                    } else {
                        error!("No answer in reply structure");
                        
                        // Try to get local-description as fallback
                        if let Some(answer) = webrtcbin.property::<Option<gstreamer_webrtc::WebRTCSessionDescription>>("local-description") {
                            debug!("Got local description as fallback");
                            let answer_sdp = answer.sdp().to_string();
                            info!("Answer SDP created (fallback), length: {}", answer_sdp.len());
                            debug!("First 200 chars of answer SDP: {}", &answer_sdp.chars().take(200).collect::<String>());
                            
                            let msg = SignalingMessage::Answer {
                                sdp: answer_sdp,
                                id: peer_id.to_string(),
                            };

                            if let Ok(json) = serde_json::to_string(&msg) {
                                info!("Sending answer JSON to client: {} chars", json.len());
                                let ws_sender_clone = ws_sender.clone();
                                tokio::task::spawn_blocking(move || {
                                    let rt = tokio::runtime::Handle::current();
                                    rt.block_on(async move {
                                        if let Ok(mut sender) = ws_sender_clone.lock() {
                                            match sender.send(Message::text(json)).await {
                                                Ok(_) => info!("Answer sent to client successfully (fallback)"),
                                                Err(e) => error!("Failed to send answer: {:?}", e),
                                            }
                                        } else {
                                            error!("Failed to lock WebSocket sender");
                                        }
                                    });
                                });
                            } else {
                                error!("Failed to serialize answer message");
                            }
                        }
                    }
                }
            }
            _ => {
                error!("Promise failed or was interrupted");
            }
        }
        
        Ok(String::new())
    }

    fn create_pipeline(
        &self,
        peer_id: &str,
        ws_sender: Arc<Mutex<futures::stream::SplitSink<WebSocket, Message>>>,
    ) -> anyhow::Result<gstreamer::Pipeline> {
        let pipeline_str = format!(
            "appsrc name=videosrc caps=video/x-raw,format=RGB,width=640,height=480,framerate=30/1 \
             is-live=true format=time ! \
             videoconvert ! \
             vp8enc deadline=1 ! \
             rtpvp8pay ! \
             application/x-rtp,media=video,encoding-name=VP8,payload=96 ! \
             webrtcbin name=webrtc bundle-policy=max-bundle"
        );

        let pipeline = gstreamer::parse::launch(&pipeline_str)?
            .dynamic_cast::<gstreamer::Pipeline>()
            .map_err(|_| anyhow::anyhow!("Failed to create pipeline"))?;
        
        // Monitor pipeline messages
        let bus = pipeline.bus().unwrap();
        let peer_id_for_bus = peer_id.to_string();
        std::thread::spawn(move || {
            for msg in bus.iter_timed(gstreamer::ClockTime::NONE) {
                use gstreamer::MessageView;
                match msg.view() {
                    MessageView::StateChanged(s) => {
                        debug!("Pipeline {} element state changed: {:?} -> {:?}", 
                            peer_id_for_bus, s.old(), s.current());
                    }
                    MessageView::Error(err) => {
                        error!("Pipeline {} error: {} ({})", 
                            peer_id_for_bus, err.error(), err.debug().unwrap_or_default());
                    }
                    MessageView::Warning(warn) => {
                        warn!("Pipeline {} warning: {} ({})", 
                            peer_id_for_bus, warn.error(), warn.debug().unwrap_or_default());
                    }
                    MessageView::Eos(_) => {
                        info!("Pipeline {} reached EOS", peer_id_for_bus);
                        break;
                    }
                    _ => {}
                }
            }
        });

        let webrtcbin = pipeline
            .by_name("webrtc")
            .context("Failed to get webrtcbin")?;

        webrtcbin.set_property_from_str("stun-server", "stun://stun.l.google.com:19302");

        let peer_id_clone = peer_id.to_string();
        let _ws_sender_clone = ws_sender.clone();
        webrtcbin.connect("on-negotiation-needed", false, move |_| {
            debug!("Negotiation needed for {}", peer_id_clone);
            None
        });

        let peer_id_clone = peer_id.to_string();
        let ws_sender_clone = ws_sender.clone();
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            // Handle potential errors in callback to avoid panic
            let _webrtc = match values[0].get::<gstreamer::Element>() {
                Ok(elem) => elem,
                Err(_) => {
                    error!("Failed to get webrtc element from values");
                    return None;
                }
            };
            
            let mline_index = match values[1].get::<u32>() {
                Ok(idx) => idx,
                Err(_) => {
                    error!("Failed to get mline_index from values");
                    return None;
                }
            };
            
            let candidate = match values[2].get::<String>() {
                Ok(cand) => cand,
                Err(_) => {
                    error!("Failed to get candidate from values");
                    return None;
                }
            };

            let msg = SignalingMessage::Ice {
                candidate,
                sdp_mline_index: mline_index,
                id: peer_id_clone.clone(),
            };

            if let Ok(json) = serde_json::to_string(&msg) {
                let ws_sender = ws_sender_clone.clone();
                
                // Use std::thread instead of tokio::task::spawn_blocking to avoid potential runtime issues
                std::thread::spawn(move || {
                    // Try to get current runtime, or create a new one if needed
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.block_on(async move {
                            if let Ok(mut sender) = ws_sender.lock() {
                                let _ = sender.send(Message::text(json)).await;
                            }
                        });
                    } else {
                        // If no runtime is available, just log the error
                        error!("No tokio runtime available for sending ICE candidate");
                    }
                });
            }

            None
        });

        let peer_id_clone = peer_id.to_string();
        let _ws_sender_clone = ws_sender.clone();
        webrtcbin.connect("notify::ice-connection-state", false, move |values| {
            let _webrtc = match values[0].get::<gstreamer::Element>() {
                Ok(elem) => elem,
                Err(_) => {
                    error!("Failed to get webrtc element from values");
                    return None;
                }
            };
            
            let state = _webrtc.property::<WebRTCICEConnectionState>("ice-connection-state");
            info!("ICE connection state for {}: {:?}", peer_id_clone, state);
            
            // Also check connection state
            let conn_state = _webrtc.property::<gstreamer_webrtc::WebRTCPeerConnectionState>("connection-state");
            info!("Peer connection state for {}: {:?}", peer_id_clone, conn_state);
            None
        });


        Ok(pipeline)
    }

    fn add_ice_candidate(&self, peer_id: &str, candidate: &str, mline_index: u32) -> anyhow::Result<()> {
        debug!("Adding ICE candidate for peer {}: {}", peer_id, candidate);
        let peers = self.peers.lock().unwrap();
        if let Some(peer) = peers.get(peer_id) {
            peer.webrtcbin
                .emit_by_name::<()>("add-ice-candidate", &[&mline_index, &candidate]);
            debug!("ICE candidate added successfully");
        } else {
            warn!("Peer {} not found when adding ICE candidate", peer_id);
        }
        Ok(())
    }

    fn remove_peer(&self, peer_id: &str) {
        let mut peers = self.peers.lock().unwrap();
        if let Some(peer) = peers.remove(peer_id) {
            let _ = peer.pipeline.set_state(gstreamer::State::Null);
        }
    }

    fn send_frame_to_peers(&self, frame_data: &[u8]) {
        let peers = self.peers.lock().unwrap();
        let peer_count = peers.len();
        if peer_count > 0 {
            debug!("Sending frame (size: {} bytes) to {} peers", frame_data.len(), peer_count);
        }
        for (peer_id, peer) in peers.iter() {
            let mut buffer = gstreamer::Buffer::from_slice(frame_data.to_vec());
            
            // Set the buffer timestamp
            let buffer_ref = buffer.get_mut().unwrap();
            let clock = gstreamer::SystemClock::obtain();
            let base_time = peer.pipeline.base_time();
            let now = clock.time();
            if let (Some(now), Some(base_time)) = (now, base_time) {
                buffer_ref.set_pts(now - base_time);
            }
            
            if let Err(e) = peer.appsrc.push_buffer(buffer) {
                warn!("Failed to push buffer to peer {}: {}", peer_id, e);
            }
        }
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    env_logger::init();
    gstreamer::init()?;

    let (frame_sender, mut frame_receiver) = mpsc::channel::<Vec<u8>>(1);
    let server = Arc::new(WebRTCServer::new(frame_sender));

    let server_clone = server.clone();
    tokio::spawn(async move {
        while let Some(frame) = frame_receiver.recv().await {
            server_clone.send_frame_to_peers(&frame);
        }
    });

    let server_clone = server.clone();
    let websocket_route = warp::path("ws")
        .and(warp::ws())
        .map(move |ws: warp::ws::Ws| {
            let server = server_clone.clone();
            ws.on_upgrade(move |websocket| async move {
                let id = Uuid::new_v4().to_string();
                info!("New client connected: {}", id);
                server.handle_websocket(websocket, id).await;
            })
        });

    let port = env::var("SIGNALING_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .map_err(|e| eyre::eyre!("Invalid SIGNALING_PORT: {}", e))?;

    let server_task = tokio::spawn(async move {
        info!("WebRTC signaling server listening on port {}", port);
        warp::serve(websocket_route)
            .run(([0, 0, 0, 0], port))
            .await;
    });

    let (_node, mut events) = DoraNode::init_from_env()?;
    
    while let Some(event) = events.next().await {
        match event {
            Event::Input { id, data, metadata } => {
                if id.as_str() == "image" {
                    let encoding = if let Some(param) = metadata.parameters.get("encoding") {
                        match param {
                            dora_node_api::Parameter::String(s) => s.to_lowercase(),
                            _ => "rgb8".to_string(),
                        }
                    } else {
                        "rgb8".to_string()
                    };

                    if encoding == "rgb8" {
                        if let Some(data_arr) = data.as_any().downcast_ref::<UInt8Array>() {
                            let bytes = data_arr.values().to_vec();
                            let _ = server.frame_sender.send(bytes).await;
                        } else {
                            warn!("Failed to convert ArrowData to UInt8Array");
                        }
                    } else {
                        warn!("Unsupported image encoding: {}", encoding);
                    }
                }
            }
            Event::Stop(_) => {
                info!("Received stop event");
                break;
            }
            _ => {}
        }
    }

    server_task.abort();
    Ok(())
}
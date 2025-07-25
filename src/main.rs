use arrow::array::UInt8Array;
use dora_node_api::{DoraNode, Event};
use futures::stream::StreamExt;
use log::info;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;
use warp::Filter;

mod peer_connection;
mod signaling;
mod webrtc_server;

use webrtc_server::WebRTCServer;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    env_logger::init();
    gstreamer::init()?;

    let (frame_sender, mut frame_receiver) = mpsc::channel::<Vec<u8>>(1);
    let frame_sender_clone = frame_sender.clone();
    let server = Arc::new(WebRTCServer::new(frame_sender_clone));

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
                            let _ = frame_sender.send(bytes).await;
                        }
                    }
                }
            }
            Event::Stop(_) => {
                break;
            }
            _ => {}
        }
    }

    server_task.abort();
    Ok(())
}
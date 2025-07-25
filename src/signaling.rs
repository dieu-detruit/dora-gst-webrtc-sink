use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SignalingMessage {
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
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;

#[derive(Debug, Clone)]
pub struct PeerConnection {
    pub _id: String,
    pub pipeline: gstreamer::Pipeline,
    pub webrtcbin: gstreamer::Element,
    pub appsrc: AppSrc,
}

impl PeerConnection {
    pub fn new(
        id: String,
        pipeline: gstreamer::Pipeline,
        webrtcbin: gstreamer::Element,
        appsrc: AppSrc,
    ) -> Self {
        Self {
            _id: id,
            pipeline,
            webrtcbin,
            appsrc,
        }
    }

    pub fn send_frame(&self, frame_data: &[u8]) -> Result<(), gstreamer::FlowError> {
        let mut buffer = gstreamer::Buffer::from_slice(frame_data.to_vec());
        
        // Set the buffer timestamp
        let buffer_ref = buffer.get_mut().unwrap();
        let clock = gstreamer::SystemClock::obtain();
        let base_time = self.pipeline.base_time();
        let now = clock.time();
        if let (Some(now), Some(base_time)) = (now, base_time) {
            buffer_ref.set_pts(now - base_time);
        }
        
        match self.appsrc.push_buffer(buffer) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn shutdown(&self) {
        let _ = self.pipeline.set_state(gstreamer::State::Null);
    }
}
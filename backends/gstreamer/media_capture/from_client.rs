use media_stream::GStreamerMediaStream;
use servo_media_streams::registry::MediaStreamId;
use source::client::register_servo_media_client_src;
use std::collections::HashMap;
use std::sync::Mutex;

lazy_static! {
    static ref CAPTURE_SOURCE_REGISTRY: Mutex<HashMap<MediaStreamId, ClientCaptureSource>> =
        Mutex::new(HashMap::new());
}

fn register_capture_source(source: ClientCaptureSource) {
    CAPTURE_SOURCE_REGISTRY
        .lock()
        .unwrap()
        .insert(source.id.unwrap(), source);
}

fn unregister_capture_source(source: &MediaStreamId) {
    CAPTURE_SOURCE_REGISTRY.lock().unwrap().remove(source);
}

pub fn get_capture_source(source: &MediaStreamId) -> Option<ClientCaptureSource> {
    CAPTURE_SOURCE_REGISTRY.lock().unwrap().get(source).cloned()
}

#[derive(Clone)]
pub struct ClientCaptureSource {
    pub source: gst::Element,
    pub id: Option<MediaStreamId>,
}

unsafe impl Send for ClientCaptureSource {}
unsafe impl Sync for ClientCaptureSource {}

impl ClientCaptureSource {
    fn new() -> ClientCaptureSource {
        ClientCaptureSource {
            source: gst::ElementFactory::make("ServoMediaClientSrc", None).unwrap(),
            id: None,
        }
    }
}

impl Drop for ClientCaptureSource {
    fn drop(&mut self) {
        if let Some(ref id) = self.id {
            unregister_capture_source(id);
        }
    }
}

pub fn create_client_capture_stream() -> Result<MediaStreamId, glib::BoolError> {
    register_servo_media_client_src()?;

    let mut client_capture = ClientCaptureSource::new();

    let stream_id = GStreamerMediaStream::create_video_from(client_capture.source.clone());
    client_capture.id = Some(stream_id.clone());
    register_capture_source(client_capture);

    Ok(stream_id)
}

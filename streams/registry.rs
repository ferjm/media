use super::MediaStream;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

lazy_static! {
    static ref MEDIA_STREAMS_REGISTRY: Mutex<HashMap<MediaStreamId, Arc<Mutex<dyn MediaStream>>>> =
        Mutex::new(HashMap::new());
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct MediaStreamId(Uuid);
impl MediaStreamId {
    pub fn new() -> MediaStreamId {
        Self { 0: Uuid::new_v4() }
    }

    pub fn id(self) -> Uuid {
        self.0
    }
}

impl Serialize for MediaStreamId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:p}", &self.0.to_string()))
    }
}

impl<'de> Deserialize<'de> for MediaStreamId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<MediaStreamId, D::Error> {
        let value: &str = Deserialize::deserialize(d)?;
        let uuid = Uuid::from_str(value).map_err(D::Error::custom)?;
        Ok(MediaStreamId(uuid))
    }
}

pub fn register_stream(stream: Arc<Mutex<dyn MediaStream>>) -> MediaStreamId {
    let id = MediaStreamId::new();
    stream.lock().unwrap().set_id(id.clone());
    MEDIA_STREAMS_REGISTRY
        .lock()
        .unwrap()
        .insert(id.clone(), stream);
    id
}

pub fn unregister_stream(stream: &MediaStreamId) {
    MEDIA_STREAMS_REGISTRY.lock().unwrap().remove(stream);
}

pub fn get_stream(stream: &MediaStreamId) -> Option<Arc<Mutex<dyn MediaStream>>> {
    MEDIA_STREAMS_REGISTRY.lock().unwrap().get(stream).cloned()
}

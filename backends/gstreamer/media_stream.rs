use super::BACKEND_BASE_TIME;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_app::AppSrc;
use servo_media_streams::registry::{
    get_stream, register_stream, unregister_stream, MediaStreamId,
};
use servo_media_streams::{MediaOutput, MediaSocket, MediaStream, MediaStreamType};
use std::any::Any;
use std::sync::{Arc, Mutex};

lazy_static! {
    pub static ref RTP_CAPS_OPUS: gst::Caps = {
        gst::Caps::new_simple(
            "application/x-rtp",
            &[("media", &"audio"), ("encoding-name", &"OPUS")],
        )
    };
    pub static ref RTP_CAPS_VP8: gst::Caps = {
        gst::Caps::new_simple(
            "application/x-rtp",
            &[("media", &"video"), ("encoding-name", &"VP8")],
        )
    };
}

pub struct GStreamerMediaStream {
    id: Option<MediaStreamId>,
    type_: MediaStreamType,
    elements: Vec<gst::Element>,
    pipeline: Option<gst::Pipeline>,
}

impl MediaStream for GStreamerMediaStream {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_mut_any(&mut self) -> &mut dyn Any {
        self
    }

    fn set_id(&mut self, id: MediaStreamId) {
        self.id = Some(id);
    }

    fn ty(&self) -> MediaStreamType {
        self.type_
    }

    fn push_data(&self, data: Vec<u8>) {
        if let Some(source) = self.elements.first() {
            if let Some(appsrc) = source.downcast_ref::<AppSrc>() {
                let buffer = gst::Buffer::from_slice(data);
                if let Err(error) = appsrc.push_buffer(buffer) {
                    warn!("{}", error);
                }
            }
        }
    }
}

impl GStreamerMediaStream {
    pub fn new(type_: MediaStreamType, elements: Vec<gst::Element>) -> Self {
        Self {
            id: None,
            type_,
            elements,
            pipeline: None,
        }
    }

    pub fn caps(&self) -> &gst::Caps {
        match self.type_ {
            MediaStreamType::Audio => &*RTP_CAPS_OPUS,
            MediaStreamType::Video => &*RTP_CAPS_VP8,
        }
    }

    pub fn caps_with_payload(&self, payload: i32) -> gst::Caps {
        match self.type_ {
            MediaStreamType::Audio => gst::Caps::new_simple(
                "application/x-rtp",
                &[
                    ("media", &"audio"),
                    ("encoding-name", &"OPUS"),
                    ("payload", &(payload)),
                ],
            ),
            MediaStreamType::Video => gst::Caps::new_simple(
                "application/x-rtp",
                &[
                    ("media", &"video"),
                    ("encoding-name", &"VP8"),
                    ("payload", &(payload)),
                ],
            ),
        }
    }

    pub fn src_element(&self) -> gst::Element {
        self.elements.last().unwrap().clone()
    }

    pub fn attach_to_pipeline(&mut self, pipeline: &gst::Pipeline) {
        assert!(self.pipeline.is_none());
        let elements: Vec<_> = self.elements.iter().collect();
        pipeline.add_many(&elements[..]).unwrap();
        gst::Element::link_many(&elements[..]).unwrap();
        for element in elements {
            element.sync_state_with_parent().unwrap();
        }
        self.pipeline = Some(pipeline.clone());
    }

    pub fn pipeline_or_new(&mut self) -> gst::Pipeline {
        if let Some(ref pipeline) = self.pipeline {
            pipeline.clone()
        } else {
            let pipeline = gst::Pipeline::new(Some("gstreamermediastream fresh pipeline"));
            let clock = gst::SystemClock::obtain();
            pipeline.set_start_time(gst::ClockTime::none());
            pipeline.set_base_time(*BACKEND_BASE_TIME);
            pipeline.use_clock(Some(&clock));
            self.attach_to_pipeline(&pipeline);
            pipeline
        }
    }

    pub fn create_video() -> MediaStreamId {
        let videotestsrc = gst::ElementFactory::make("videotestsrc", None).unwrap();
        videotestsrc.set_property_from_str("pattern", "ball");
        videotestsrc
            .set_property("is-live", &true)
            .expect("videotestsrc doesn't have expected 'is-live' property");

        Self::create_video_from(videotestsrc)
    }

    /// Attaches encoding adapters to the stream, returning the source element
    pub fn encoded(&mut self) -> gst::Element {
        let pipeline = self
            .pipeline
            .as_ref()
            .expect("GStreamerMediaStream::encoded() should not be called without a pipeline");
        let src = self.src_element();

        let capsfilter = gst::ElementFactory::make("capsfilter", None).unwrap();
        capsfilter.set_property("caps", self.caps()).unwrap();
        match self.type_ {
            MediaStreamType::Video => {
                let vp8enc = gst::ElementFactory::make("vp8enc", None).unwrap();
                vp8enc
                    .set_property("deadline", &1i64)
                    .expect("vp8enc doesn't have expected 'deadline' property");

                let rtpvp8pay = gst::ElementFactory::make("rtpvp8pay", None).unwrap();
                let queue2 = gst::ElementFactory::make("queue", None).unwrap();

                pipeline
                    .add_many(&[&vp8enc, &rtpvp8pay, &queue2, &capsfilter])
                    .unwrap();
                gst::Element::link_many(&[&src, &vp8enc, &rtpvp8pay, &queue2, &capsfilter])
                    .unwrap();
                vp8enc.sync_state_with_parent().unwrap();
                rtpvp8pay.sync_state_with_parent().unwrap();
                queue2.sync_state_with_parent().unwrap();
                capsfilter.sync_state_with_parent().unwrap();
                capsfilter
            }
            MediaStreamType::Audio => {
                let opusenc = gst::ElementFactory::make("opusenc", None).unwrap();
                let rtpopuspay = gst::ElementFactory::make("rtpopuspay", None).unwrap();
                let queue3 = gst::ElementFactory::make("queue", None).unwrap();
                pipeline
                    .add_many(&[&opusenc, &rtpopuspay, &queue3, &capsfilter])
                    .unwrap();
                gst::Element::link_many(&[&src, &opusenc, &rtpopuspay, &queue3, &capsfilter])
                    .unwrap();
                opusenc.sync_state_with_parent().unwrap();
                rtpopuspay.sync_state_with_parent().unwrap();
                queue3.sync_state_with_parent().unwrap();
                capsfilter
            }
        }
    }

    pub fn create_video_from(source: gst::Element) -> MediaStreamId {
        let videoconvert = gst::ElementFactory::make("videoconvert", None).unwrap();
        let queue = gst::ElementFactory::make("queue", None).unwrap();

        register_stream(Arc::new(Mutex::new(GStreamerMediaStream::new(
            MediaStreamType::Video,
            vec![source, videoconvert, queue],
        ))))
    }

    pub fn create_audio() -> MediaStreamId {
        let audiotestsrc = gst::ElementFactory::make("audiotestsrc", None).unwrap();
        audiotestsrc.set_property_from_str("wave", "sine");
        audiotestsrc
            .set_property("is-live", &true)
            .expect("audiotestsrc doesn't have expected 'is-live' property");

        Self::create_audio_from(audiotestsrc)
    }

    pub fn create_audio_from(source: gst::Element) -> MediaStreamId {
        let queue = gst::ElementFactory::make("queue", None).unwrap();
        let audioconvert = gst::ElementFactory::make("audioconvert", None).unwrap();
        let audioresample = gst::ElementFactory::make("audioresample", None).unwrap();
        let queue2 = gst::ElementFactory::make("queue", None).unwrap();

        register_stream(Arc::new(Mutex::new(GStreamerMediaStream::new(
            MediaStreamType::Audio,
            vec![source, queue, audioconvert, audioresample, queue2],
        ))))
    }

    pub fn create_proxy(ty: MediaStreamType) -> (MediaStreamId, GstreamerMediaSocket) {
        let proxy_src = gst::ElementFactory::make("proxysrc", None).unwrap();
        let proxy_sink = gst::ElementFactory::make("proxysink", None).unwrap();
        proxy_src.set_property("proxysink", &proxy_sink).unwrap();
        let stream = match ty {
            MediaStreamType::Audio => Self::create_audio_from(proxy_src),
            MediaStreamType::Video => Self::create_video_from(proxy_src),
        };

        (stream, GstreamerMediaSocket { proxy_sink })
    }

    pub fn push_data(stream: &MediaStreamId, data: Vec<u8>) {
        let stream = get_stream(stream).expect("Media streams registry does not contain such ID");
        let mut stream = stream.lock().unwrap();
        let stream = stream
            .as_mut_any()
            .downcast_mut::<GStreamerMediaStream>()
            .unwrap();
        stream.push_data(data);
    }
}

impl Drop for GStreamerMediaStream {
    fn drop(&mut self) {
        if let Some(ref id) = self.id {
            unregister_stream(id);
        }
    }
}

pub struct MediaSink {
    streams: Vec<Arc<Mutex<dyn MediaStream>>>,
}

impl MediaSink {
    pub fn new() -> Self {
        MediaSink { streams: vec![] }
    }
}

impl MediaOutput for MediaSink {
    fn add_stream(&mut self, stream: &MediaStreamId) {
        let stream = get_stream(&stream).expect("Media streams registry does not contain such ID");
        {
            let mut stream = stream.lock().unwrap();
            let stream = stream
                .as_mut_any()
                .downcast_mut::<GStreamerMediaStream>()
                .unwrap();
            let pipeline = stream.pipeline_or_new();
            let last_element = stream.elements.last();
            let last_element = last_element.as_ref().unwrap();
            let sink = match stream.type_ {
                MediaStreamType::Audio => "autoaudiosink",
                MediaStreamType::Video => "autovideosink",
            };
            let sink = gst::ElementFactory::make(sink, None).unwrap();
            pipeline.add(&sink).unwrap();
            gst::Element::link_many(&[last_element, &sink][..]).unwrap();

            pipeline.set_state(gst::State::Playing).unwrap();
            sink.sync_state_with_parent().unwrap();
        }
        self.streams.push(stream.clone());
    }
}

pub struct GstreamerMediaSocket {
    proxy_sink: gst::Element,
}

impl GstreamerMediaSocket {
    pub fn proxy_sink(&self) -> &gst::Element {
        &self.proxy_sink
    }
}

impl MediaSocket for GstreamerMediaSocket {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

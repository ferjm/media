use glib::prelude::*;
use gst;
use gst::prelude::*;
use servo_media_webrtc::{MediaOutput, MediaStream};
use std::any::Any;

lazy_static! {
    static ref RTP_CAPS_OPUS: gst::Caps = {
        gst::Caps::new_simple(
            "application/x-rtp",
            &[
                ("media", &"audio"),
                ("encoding-name", &"OPUS"),
                ("payload", &(97i32)),
            ],
        )
    };
    static ref RTP_CAPS_VP8: gst::Caps = {
        gst::Caps::new_simple(
            "application/x-rtp",
            &[
                ("media", &"video"),
                ("encoding-name", &"VP8"),
                ("payload", &(96i32)),
            ],
        )
    };
}

#[derive(Debug, PartialEq)]
pub enum StreamType {
    Audio,
    Video,
}

pub struct GStreamerMediaStream {
    type_: StreamType,
    elements: Vec<gst::Element>,
    pipeline: Option<gst::Pipeline>,
}

impl MediaStream for GStreamerMediaStream {
    fn as_any(&self) -> &Any {
        self
    }

    fn as_mut_any(&mut self) -> &mut Any {
        self
    }
}

impl GStreamerMediaStream {
    pub fn attach_to_pipeline(&mut self, pipeline: &gst::Pipeline, webrtcbin: &gst::Element) {
        println!("atttaching a {:?} stream", self.type_);
        let elements: Vec<_> = self.elements.iter().collect();
        pipeline.add_many(&elements[..]).unwrap();
        gst::Element::link_many(&elements[..]).unwrap();
        for element in elements {
            element.sync_state_with_parent().unwrap();
        }

        let caps = match self.type_ {
            StreamType::Audio => &*RTP_CAPS_OPUS,
            StreamType::Video => &*RTP_CAPS_VP8,
        };
        self.elements
            .last()
            .as_ref()
            .unwrap()
            .link_filtered(webrtcbin, caps)
            .unwrap();
        self.pipeline = Some(pipeline.clone());
    }

    pub fn create_video() -> GStreamerMediaStream {
        let videotestsrc = gst::ElementFactory::make("videotestsrc", None).unwrap();
        videotestsrc.set_property_from_str("pattern", "ball");
        videotestsrc.set_property("is-live", &true).unwrap();

        Self::create_video_from(videotestsrc)
    }

    pub fn create_video_from(source: gst::Element) -> GStreamerMediaStream {
        let videoconvert = gst::ElementFactory::make("videoconvert", None).unwrap();
        let queue = gst::ElementFactory::make("queue", None).unwrap();
        let vp8enc = gst::ElementFactory::make("vp8enc", None).unwrap();

        vp8enc.set_property("deadline", &1i64).unwrap();

        let rtpvp8pay = gst::ElementFactory::make("rtpvp8pay", None).unwrap();
        let queue2 = gst::ElementFactory::make("queue", None).unwrap();

        GStreamerMediaStream {
            type_: StreamType::Video,
            elements: vec![source, videoconvert, queue, vp8enc, rtpvp8pay, queue2],
            pipeline: None,
        }
    }

    pub fn create_audio() -> GStreamerMediaStream {
        let audiotestsrc = gst::ElementFactory::make("audiotestsrc", None).unwrap();
        audiotestsrc.set_property_from_str("wave", "red-noise");
        audiotestsrc.set_property("is-live", &true).unwrap();

        Self::create_audio_from(audiotestsrc)
    }

    pub fn create_audio_from(source: gst::Element) -> GStreamerMediaStream {
        let queue = gst::ElementFactory::make("queue", None).unwrap();
        let audioconvert = gst::ElementFactory::make("audioconvert", None).unwrap();
        let audioresample = gst::ElementFactory::make("audioresample", None).unwrap();
        let queue2 = gst::ElementFactory::make("queue", None).unwrap();
        let opusenc = gst::ElementFactory::make("opusenc", None).unwrap();
        let rtpopuspay = gst::ElementFactory::make("rtpopuspay", None).unwrap();
        let queue3 = gst::ElementFactory::make("queue", None).unwrap();

        GStreamerMediaStream {
            type_: StreamType::Audio,
            elements: vec![
                source,
                queue,
                audioconvert,
                audioresample,
                queue2,
                opusenc,
                rtpopuspay,
                queue3,
            ],
            pipeline: None,
        }
    }

    pub fn create_stream_with_pipeline(
        type_: StreamType,
        elements: Vec<gst::Element>,
        pipeline: gst::Pipeline,
    ) -> GStreamerMediaStream {
        GStreamerMediaStream {
            type_,
            elements,
            pipeline: Some(pipeline),
        }
    }
}

pub struct MediaSink {
    streams: Vec<Box<MediaStream>>,
}

impl MediaSink {
    pub fn new() -> Self {
        MediaSink { streams: vec![] }
    }
}

impl MediaOutput for MediaSink {
    fn add_stream(&mut self, stream: Box<MediaStream>) {
        {
            let stream = stream
                .as_any()
                .downcast_ref::<GStreamerMediaStream>()
                .unwrap();
            let last_element = stream.elements.last();
            let last_element = last_element.as_ref().unwrap();
            let sink = match stream.type_ {
                StreamType::Audio => "autoaudiosink",
                StreamType::Video => "autovideosink",
            };
            let sink = gst::ElementFactory::make(sink, None).unwrap();
            stream.pipeline.as_ref().unwrap().add(&sink).unwrap();
            gst::Element::link_many(&[last_element, &sink][..]).unwrap();

            sink.sync_state_with_parent().unwrap();
        }

        self.streams.push(stream);
    }
}

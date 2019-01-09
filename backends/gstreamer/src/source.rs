use glib;
use glib::subclass;
use glib::subclass::prelude::*;
use glib::translate::*;
use gst;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_app;
use url::Url;

const MAX_SRC_QUEUE_SIZE: u64 = 50 * 1024 * 1024; // 50 MB.

// Implementation sub-module of the GObject
mod imp {
    use super::*;

    macro_rules! inner_appsrc_proxy {
        ($fn_name:ident, $return_type:ty) => (
            pub fn $fn_name(&self) -> $return_type {
                self.appsrc.$fn_name()
            }
        );

        ($fn_name:ident, $arg1:ident, $arg1_type:ty, $return_type:ty) => (
            pub fn $fn_name(&self, $arg1: $arg1_type) -> $return_type {
                self.appsrc.$fn_name($arg1)
            }
        )
    }

    // The actual data structure that stores our values. This is not accessible
    // directly from the outside.
    pub struct ServoSrc {
        cat: gst::DebugCategory,
        appsrc: gst_app::AppSrc,
        srcpad: gst::GhostPad,
    }

    impl ServoSrc {
        pub fn set_size(&self, size: i64) {
            if self.appsrc.get_size() == -1 {
                self.appsrc.set_size(size);
            }
        }

        inner_appsrc_proxy!(end_of_stream, Result<gst::FlowSuccess, gst::FlowError>);
        inner_appsrc_proxy!(get_current_level_bytes, u64);
        inner_appsrc_proxy!(get_max_bytes, u64);
        inner_appsrc_proxy!(push_buffer, buffer, gst::Buffer, Result<gst::FlowSuccess, gst::FlowError>);
        inner_appsrc_proxy!(set_callbacks, callbacks, gst_app::AppSrcCallbacks, ());
        inner_appsrc_proxy!(set_stream_type, type_, gst_app::AppStreamType, ());

        fn query(
            &self,
            pad: &gst::Pad,
            _element: &gst::Element,
            query: &mut gst::QueryRef,
        ) -> bool {
            gst_log!(self.cat, obj: pad, "Handling query {:?}", query);

            // In order to make buffering/downloading work as we want, apart from
            // setting the appropriate flags on the player playbin,
            // the source needs to either:
            //
            // 1. be an http, mms, etc. scheme
            // 2. report that it is "bandwidth limited".
            //
            // 1. is not straightforward because we are using a servosrc scheme for now.
            // This may change in the future if we end up handling http/https/data
            // URIs, which is what WebKit does.
            //
            // For 2. we need to make servosrc handle the scheduling properties query
            // to report that it "is bandwidth limited".
            let ret = match query.view_mut() {
                gst::QueryView::Scheduling(ref mut q) => {
                    let flags =
                        gst::SchedulingFlags::SEQUENTIAL | gst::SchedulingFlags::BANDWIDTH_LIMITED;
                    q.set(flags, 1, -1, 0);
                    q.add_scheduling_modes(&[gst::PadMode::Push]);
                    true
                }
                _ => false,
            };
            let ret = if !ret {
                pad.query_default(None::<&gst::Pad>, query)
            } else {
                true
            };

            if ret {
                gst_log!(self.cat, obj: pad, "Handled query {:?}", query);
            } else {
                gst_log!(self.cat, obj: pad, "Didn't handle query {:?}", query);
            }
            ret
        }
    }

    // Basic declaration of our type for the GObject type system
    impl ObjectSubclass for ServoSrc {
        const NAME: &'static str = "ServoSrc";
        type ParentType = gst::Bin;
        type Instance = gst::subclass::ElementInstanceStruct<Self>;
        type Class = subclass::simple::ClassStruct<Self>;

        glib_object_subclass!();

        // Called once at the very beginning of instantiation of each instance and
        // creates the data structure that contains all our state
        fn new_with_class(klass: &subclass::simple::ClassStruct<Self>) -> Self {
            let app_src = gst::ElementFactory::make("appsrc", None)
                .map(|elem| elem.downcast::<gst_app::AppSrc>().unwrap())
                .expect("Could not create appsrc element");

            let pad_templ = klass.get_pad_template("src").unwrap();
            let ghost_pad = gst::GhostPad::new_no_target_from_template("src", &pad_templ).unwrap();

            ghost_pad.set_query_function(|pad, parent, query| {
                ServoSrc::catch_panic_pad_function(
                    parent,
                    || false,
                    |queue, element| queue.query(pad.upcast_ref::<gst::Pad>(), element, query),
                )
            });

            Self {
                cat: gst::DebugCategory::new(
                    "servosrc",
                    gst::DebugColorFlags::empty(),
                    "Servo source",
                ),
                appsrc: app_src,
                srcpad: ghost_pad,
            }
        }

        // Adds interface implemenations in the class
        fn type_init(type_: &mut subclass::InitializingType<Self>) {
            type_.add_interface::<gst::URIHandler>();
        }

        // Called exactly once before the first instantiation of an instance. This
        // sets up any type-specific things, in this specific case it installs the
        // properties so that GObject knows about their existence and they can be
        // used on instances of our type
        fn class_init(klass: &mut subclass::simple::ClassStruct<Self>) {
            klass.set_metadata(
                "Servo Media Source",
                "Source/Audio/Video",
                "Feed player with media data",
                "Servo developers",
            );

            let caps = gst::Caps::new_any();
            let src_pad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();
            klass.add_pad_template(src_pad_template);
        }
    }

    // The ObjectImpl trait provides the setters/getters for GObject properties.
    // Here we need to provide the values that are internally stored back to the
    // caller, or store whatever new value the caller is providing.
    //
    // This maps between the GObject properties and our internal storage of the
    // corresponding values of the properties.
    impl ObjectImpl for ServoSrc {
        glib_object_impl!();

        // Called right after construction of a new instance
        fn constructed(&self, obj: &glib::Object) {
            // Call the parent class' ::constructed() implementation first
            self.parent_constructed(obj);

            let bin = obj.downcast_ref::<gst::Bin>().unwrap();
            self.add_element(bin, &self.appsrc.clone().upcast())
                .expect("Could not add appsrc element to bin");

            let target_pad = self
                .appsrc
                .get_static_pad("src")
                .expect("Could not get source pad");
            self.srcpad.set_target(&target_pad).unwrap();

            let element = obj.downcast_ref::<gst::Element>().unwrap();
            element
                .add_pad(&self.srcpad)
                .expect("Could not add source pad to bin");

            self.appsrc.set_max_bytes(MAX_SRC_QUEUE_SIZE);
            self.appsrc.set_property_block(false);
            self.appsrc.set_property_format(gst::Format::Bytes);

            ::set_element_flags(element, gst::ElementFlags::SOURCE);
        }
    }

    // Implementation of gst::Element virtual methods
    impl ElementImpl for ServoSrc {}

    // Implementation of gst::Bin virtual methods
    impl BinImpl for ServoSrc {}

    impl URIHandlerImpl for ServoSrc {
        fn get_uri(&self, _element: &gst::URIHandler) -> Option<String> {
            Some("servosrc://".to_string())
        }

        fn set_uri(
            &self,
            _element: &gst::URIHandler,
            uri: Option<String>,
        ) -> Result<(), glib::Error> {
            if let Some(ref uri) = uri {
                if let Ok(uri) = Url::parse(uri) {
                    if uri.scheme() == "servosrc" {
                        return Ok(());
                    }
                }
            }
            Err(glib::Error::new(
                gst::URIError::BadUri,
                format!("Invalid URI '{:?}'", uri,).as_str(),
            ))
        }

        fn get_uri_type() -> gst::URIType {
            gst::URIType::Src
        }

        fn get_protocols() -> Vec<String> {
            vec!["servosrc".into()]
        }
    }
}

// Public part of the ServoSrc type. This behaves like a normal
// GObject binding
glib_wrapper! {
    pub struct ServoSrc(Object<gst::subclass::ElementInstanceStruct<imp::ServoSrc>,
                        subclass::simple::ClassStruct<imp::ServoSrc>, ServoSrcClass>)
        @extends gst::Bin, gst::Element, gst::Object, @implements gst::URIHandler;

    match fn {
        get_type => || imp::ServoSrc::get_type().to_glib(),
    }
}

unsafe impl Send for ServoSrc {}
unsafe impl Sync for ServoSrc {}

macro_rules! inner_servosrc_proxy {
    ($fn_name:ident, $return_type:ty) => (
        pub fn $fn_name(&self) -> $return_type {
            imp::ServoSrc::from_instance(self).$fn_name()
        }
    );

    ($fn_name:ident, $arg1:ident, $arg1_type:ty, $return_type:ty) => (
        pub fn $fn_name(&self, $arg1: $arg1_type) -> $return_type {
            imp::ServoSrc::from_instance(self).$fn_name($arg1)
        }
    )
}

impl ServoSrc {
    pub fn set_size(&self, size: i64) {
        imp::ServoSrc::from_instance(self).set_size(size)
    }

    inner_servosrc_proxy!(end_of_stream, Result<gst::FlowSuccess, gst::FlowError>);
    inner_servosrc_proxy!(get_current_level_bytes, u64);
    inner_servosrc_proxy!(get_max_bytes, u64);
    inner_servosrc_proxy!(push_buffer, buffer, gst::Buffer, Result<gst::FlowSuccess, gst::FlowError>);
    inner_servosrc_proxy!(set_callbacks, callbacks, gst_app::AppSrcCallbacks, ());
    inner_servosrc_proxy!(set_stream_type, type_, gst_app::AppStreamType, ());
}

// Registers the type for our element, and then registers in GStreamer
// under the name "servosrc" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register_servo_src() -> Result<(), glib::BoolError> {
    gst::Element::register(None, "servosrc", 0, ServoSrc::static_type())
}

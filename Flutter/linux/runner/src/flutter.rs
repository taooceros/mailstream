//! Flutter's Linux embedder (`libflutter_linux_gtk.so`) through the bindings build.rs makes of its
//! headers, with owning wrappers for method channels, their calls and the values they carry.
use std::ffi::{CStr, CString, c_void};

use glib::gobject_ffi::{g_object_ref, g_object_unref};
use glib::translate::ToGlibPtr;

/// The bindgen output: Flutter's own items, over gtk-rs's GLib, GIO and GTK types.
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, unused_imports)]
pub mod sys {
    use gdk::ffi::*;
    use gio::ffi::*;
    use glib::ffi::*;
    use glib::gobject_ffi::*;
    use gtk::ffi::*;

    include!(concat!(env!("OUT_DIR"), "/flutter_linux.rs"));
}

use sys::{FlMethodCall, FlMethodChannel, FlValue};

fn text(value: &str) -> CString {
    CString::new(value.replace('\0', "")).expect("NULs removed")
}

/// A value this side owns.
pub struct Value(*mut FlValue);

impl Value {
    pub fn bool(value: bool) -> Self {
        Self(unsafe { sys::fl_value_new_bool(value.into()) })
    }

    /// A list of strings: paths, for the files channel.
    pub fn strings<S: AsRef<str>>(items: &[S]) -> Self {
        let list = unsafe { sys::fl_value_new_list() };
        for item in items {
            let item = text(item.as_ref());
            unsafe { sys::fl_value_append_take(list, sys::fl_value_new_string(item.as_ptr())) };
        }
        Self(list)
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        unsafe { sys::fl_value_unref(self.0) };
    }
}

/// One call from Dart, answered once (a call dropped unanswered gets Flutter's own error).
pub struct MethodCall(*mut FlMethodCall);

impl MethodCall {
    /// A reference of its own to a call the embedder handed over.
    unsafe fn retain(call: *mut FlMethodCall) -> Self {
        Self(unsafe { g_object_ref(call.cast()) }.cast())
    }

    pub fn method(&self) -> &str {
        unsafe { CStr::from_ptr(sys::fl_method_call_get_name(self.0)) }.to_str().unwrap_or_default()
    }

    fn args(&self, kind: sys::FlValueType) -> Option<*mut FlValue> {
        let args = unsafe { sys::fl_method_call_get_args(self.0) };
        (!args.is_null() && unsafe { sys::fl_value_get_type(args) } == kind).then_some(args)
    }

    pub fn bool_argument(&self) -> Option<bool> {
        self.args(sys::FL_VALUE_TYPE_BOOL).map(|args| unsafe { sys::fl_value_get_bool(args) })
    }

    pub fn string_argument(&self) -> Option<String> {
        self.args(sys::FL_VALUE_TYPE_STRING)
            .map(|args| unsafe { CStr::from_ptr(sys::fl_value_get_string(args)) }.to_string_lossy().into_owned())
    }

    pub fn succeed(self, result: Option<Value>) {
        let result = result.as_ref().map_or(std::ptr::null_mut(), |value| value.0);
        unsafe { sys::fl_method_call_respond_success(self.0, result, std::ptr::null_mut()) };
    }

    pub fn fail(self, code: &str, message: &str) {
        let (code, message) = (text(code), text(message));
        unsafe {
            sys::fl_method_call_respond_error(
                self.0,
                code.as_ptr(),
                message.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
    }

    pub fn not_implemented(self) {
        unsafe { sys::fl_method_call_respond_not_implemented(self.0, std::ptr::null_mut()) };
    }
}

impl Drop for MethodCall {
    fn drop(&mut self) {
        unsafe { g_object_unref(self.0.cast()) };
    }
}

/// A method channel on a view's engine, with the standard codec. Dropping it stops its handler.
pub struct MethodChannel(*mut FlMethodChannel);

impl MethodChannel {
    /// `view` is the `FlView`.
    pub fn new(view: &gtk::Widget, name: &str) -> Self {
        let name = text(name);
        unsafe {
            let view: *mut gtk::ffi::GtkWidget = view.to_glib_none().0;
            let messenger = sys::fl_engine_get_binary_messenger(sys::fl_view_get_engine(view.cast()));
            let codec = sys::fl_standard_method_codec_new();
            let channel = sys::fl_method_channel_new(messenger, name.as_ptr(), codec.cast());
            g_object_unref(codec.cast());
            Self(channel)
        }
    }

    /// Calls `handler` with each call from Dart, on the main thread.
    pub fn handle(&self, handler: impl Fn(MethodCall) + 'static) {
        type Boxed = Box<dyn Fn(MethodCall)>;
        unsafe extern "C" fn call(_channel: *mut FlMethodChannel, call: *mut FlMethodCall, data: *mut c_void) {
            let handler = unsafe { &*data.cast::<Boxed>() };
            handler(unsafe { MethodCall::retain(call) });
        }
        unsafe extern "C" fn free(data: *mut c_void) {
            drop(unsafe { Box::from_raw(data.cast::<Boxed>()) });
        }
        let boxed: Box<Boxed> = Box::new(Box::new(handler));
        unsafe {
            sys::fl_method_channel_set_method_call_handler(self.0, Some(call), Box::into_raw(boxed).cast(), Some(free))
        };
    }

    /// Calls `method` in Dart, ignoring its answer.
    pub fn invoke(&self, method: &str, args: Value) {
        let method = text(method);
        unsafe {
            sys::fl_method_channel_invoke_method(
                self.0,
                method.as_ptr(),
                args.0,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            )
        };
    }
}

impl Drop for MethodChannel {
    fn drop(&mut self) {
        unsafe {
            sys::fl_method_channel_set_method_call_handler(self.0, None, std::ptr::null_mut(), None);
            g_object_unref(self.0.cast());
        }
    }
}

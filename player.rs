use std::cast;
use std::logging;
use std::ptr;
use std::str::raw::from_c_str;
use std::task;

use gtk::*;
use gtk::ffi::*;

use gui;

struct ClockIDWrapper {
    ci: GstClockID,
}

impl ClockIDWrapper {
    fn new(ci: GstClockID) -> ClockIDWrapper {
        ClockIDWrapper { ci: ci }
    }
}

impl Drop for ClockIDWrapper {
    fn drop(&mut self) {
        unsafe {
            gst_clock_id_unschedule(self.ci);
            gst_clock_id_unref(self.ci);
        }
    }
}

struct Player {
    initialized: bool,

    uri_set: bool,
    playing: bool,

    playbin: *mut GstElement,
    report_clock_id: Option<ClockIDWrapper>,
    progress_clock_id: Option<ClockIDWrapper>,
}

impl Player {
    pub fn new() -> Player {
        Player {
            initialized: false,
            uri_set: false,
            playing: false,
            playbin: ptr::mut_null(),
            report_clock_id: None,
            progress_clock_id: None,
        }
    }

    // It's important that the `gui` pointer be constant for the entire duration
    // of the program, as it's sent into the gstreamer lib.
    // I know, this is <strike>quite</strike> very hacky.
    pub fn init(&mut self, args: ~[~str], gui: &gui::Gui) -> ~[~str] {
        let args2 = unsafe {
            gst_init_with_args(args)
        };
        unsafe {
            "playbin".with_c_str(|c_str| {
                "rusttracks-playbin".with_c_str(|rtpb| {
                    self.playbin = gst_element_factory_make(c_str, rtpb);
                });
            });
            if self.playbin.is_null() {
                fail!("failed to create playbin");
            }

            let bus = gst_pipeline_get_bus(cast::transmute(self.playbin));
            gst_bus_add_watch(bus, bus_callback,
                              cast::transmute::<&gui::Gui, gpointer>(gui));
        }
        self.initialized = true;
        args2
    }

    pub fn set_uri(&mut self, uri: &str, gui: &gui::Gui) {
        self.stop();
        unsafe {
            "uri".with_c_str(|property_c_str| {
                uri.with_c_str(|uri_c_str| {
                    g_object_set(cast::transmute(self.playbin),
                        property_c_str, uri_c_str, ptr::null::<gchar>());
                });
            });
        }

        self.start_report_watch(gui);
        self.start_progress_watch(gui);

        self.uri_set = true;
    }

    fn start_report_watch(&mut self, gui: &gui::Gui) {
        let chan = gui.get_chan().clone();
        unsafe {

            let clock = gst_pipeline_get_clock(cast::transmute(self.playbin));

            // in nanoseconds
            let timeout: guint64 = 30 * 1000 * 1000 * 1000;
            let target_time = gst_clock_get_time(clock) + timeout;

            let ci = gst_clock_new_single_shot_id(clock, target_time);
            self.report_clock_id = Some(ClockIDWrapper::new(ci));

            do task::spawn_sched(task::SingleThreaded) {
                let res = gst_clock_id_wait(ci, ptr::mut_null());
                match res {
                    GST_CLOCK_UNSCHEDULED => { } // Ignore, nothing to do
                    GST_CLOCK_OK => {
                        debug!("30s are up! sending ReportCurrentTrack to gui");
                        chan.send(gui::ReportCurrentTrack);
                    }
                    _ => unreachable!()
                }
            }

            gst_object_unref(cast::transmute(clock));
        }
    }

    fn start_progress_watch(&mut self, gui: &gui::Gui) {
        let chan = gui.get_chan().clone();
        unsafe {

            let clock = gst_pipeline_get_clock(cast::transmute(self.playbin));

            // in nanoseconds
            let period: guint64 = 1 * 1000 * 1000 * 1000;
            let target_time = gst_clock_get_time(clock) + period;

            let ci = gst_clock_new_periodic_id(clock, target_time, period);
            self.progress_clock_id = Some(ClockIDWrapper::new(ci));

            let playbin = self.playbin;
            do task::spawn_sched(task::SingleThreaded) {
                loop {
                    let res = gst_clock_id_wait(ci, ptr::mut_null());
                    match res {
                        GST_CLOCK_UNSCHEDULED => {
                            // Track has ended or whatever, stop polling
                            break;
                        }
                        GST_CLOCK_OK => {
                            debug!("1s is up! sending progress");
                            let mut current_position = 0;
                            let success_position = gst_element_query_position(
                                playbin, GST_FORMAT_TIME, &mut current_position);
                            let mut current_duration = 0;
                            let success_duration = gst_element_query_duration(
                                playbin, GST_FORMAT_TIME, &mut current_duration);

                            if success_duration != 0 && success_position != 0 {
                                chan.send(gui::SetProgress(Some((current_position, current_duration))));
                            } else {
                                chan.send(gui::SetProgress(None));
                            }
                        }
                        _ => unreachable!()
                    }
                }
            }

            gst_object_unref(cast::transmute(clock));
        }
    }

    pub fn play(&mut self) {
        if !self.initialized {
            fail!("player is not initialized");
        }
        if !self.uri_set {
            debug!("uri not set, not playing");
            return;
        }
        unsafe {
            gst_element_set_state(self.playbin, GST_STATE_PLAYING);
        }
        self.playing = true;
    }

    pub fn pause(&mut self) {
        if !self.initialized {
            fail!("player is not initialized");
        }
        unsafe {
            gst_element_set_state(self.playbin, GST_STATE_PAUSED);
        }
        self.playing = false;
    }

    pub fn stop(&mut self) {
        if !self.initialized {
            fail!("player is not initialized");
        }
        self.report_clock_id = None;
        self.progress_clock_id = None;
        unsafe{
            gst_element_set_state(self.playbin, GST_STATE_READY);
        }
        self.uri_set = false;
        self.playing = false;
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause()
        } else {
            self.play()
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn can_play(&self) -> bool {
        self.uri_set
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                if !self.playbin.is_null() {
                    gst_element_set_state(self.playbin, GST_STATE_NULL);
                    gst_object_unref(cast::transmute(self.playbin));
                }
                gst_deinit();
            }
        }
    }
}

extern "C" fn bus_callback(_bus: *mut GstBus, msg: *mut GstMessage, data: gpointer) -> gboolean {
    unsafe {
    let gui: &gui::Gui = cast::transmute(data);

    let name = {
        let gst_obj = (*msg).src;
        if gst_obj.is_null() {
            ~"null-source"
        } else {
            let name_ptr = gst_object_get_name(gst_obj);
            if name_ptr.is_null() {
                ~"null-name"
            } else {
                let name = from_c_str(cast::transmute_immut_unsafe(name_ptr));
                g_free(cast::transmute(name_ptr));
                name
            }
        }
    };

    match (*msg)._type {
        GST_MESSAGE_ERROR => {
            let mut err = ptr::mut_null();
            let mut dbg_info = ptr::mut_null();

            gst_message_parse_error(msg, &mut err, &mut dbg_info);

            let err_msg = from_c_str(cast::transmute_immut_unsafe((*err).message));

            error!("ERROR from element {}: {}", name, err_msg);
            error!("Debugging info: {}", from_c_str(cast::transmute_immut_unsafe(dbg_info)));

            gui.get_chan().send(gui::Notify(format!("Playback error: `{}`", err_msg)));

            g_error_free(err);
            g_free(cast::transmute(dbg_info));
        }
        GST_MESSAGE_WARNING => {
            if log_enabled!(logging::WARN) {
                let mut err = ptr::mut_null();
                let mut dbg_info = ptr::mut_null();

                gst_message_parse_error(msg, &mut err, &mut dbg_info);

                warn!("WARNING from element {}: {}", name,
                    from_c_str(cast::transmute_immut_unsafe((*err).message)));
                warn!("Debugging info: {}", from_c_str(cast::transmute_immut_unsafe(dbg_info)));

                g_error_free(err);
                g_free(cast::transmute(dbg_info));
            }
        }
        GST_MESSAGE_INFO => {
            if log_enabled!(logging::INFO) {
                let mut err = ptr::mut_null();
                let mut dbg_info = ptr::mut_null();

                gst_message_parse_error(msg, &mut err, &mut dbg_info);

                info!("INFO from element {}: {}", name,
                    from_c_str(cast::transmute_immut_unsafe((*err).message)));
                info!("Debugging info: {}", from_c_str(cast::transmute_immut_unsafe(dbg_info)));

                g_error_free(err);
                g_free(cast::transmute(dbg_info));
            }
        }
        GST_MESSAGE_EOS => {
            debug!("EOS from element {}", name);
            gui.get_chan().send(gui::NextTrack);
        }
        _ => {
            if log_enabled!(logging::DEBUG) {
                let msg_type_cstr = gst_message_type_get_name((*msg)._type);
                let msg_type_name = ::std::str::raw::from_c_str(msg_type_cstr);
                debug!("message of type `{}` from element `{}`", msg_type_name, name);
            }
        }
    }

    // Returning 0 removes this callback
    return 1;
    }
}

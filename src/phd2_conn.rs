// See description here: https://raw.githubusercontent.com/wiki/OpenPHDGuiding/phd2/EventMonitoring.md

use std::{
    net::{TcpStream, Shutdown},
    io::*,
    sync::atomic::{AtomicBool, Ordering}, sync::{Arc, Mutex, RwLock, atomic::AtomicUsize},
    thread::{JoinHandle, spawn},
    time::Duration
};

use serde::{Serialize, Deserialize};

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "Event")]
pub enum Phd2IncomingObject {
    /// Describes the PHD and message protocol versions
    Version {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "PHDVersion")]
        /// the PHD version number
        version: String,

        #[serde(rename = "PHDSubver")]
        /// the PHD sub-version number
        sub_version: String,

        #[serde(rename = "MsgVersion")]
        /// the version number of the event message protocol.
        /// The current version is 1. We will bump this number if the message protocol changes.
        msg_version: i32,

        #[serde(rename = "OverlapSupport")]
        /// true if PHD support receiving RPC order while previous order has not been
        /// completed (default for latest version)
        overlap: bool,
    },

    /// The lock position has been established
    LockPositionSet {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "X")]
        /// lock position X-coordinate
        x: f64,

        #[serde(rename = "Y")]
        /// lock position Y-coordinate
        y: f64,
    },

    /// Calibration step
    Calibrating {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Mount")]
        /// name of the mount that was calibrated
        mount: String,

        /// calibration direction (phase)
        dir: String,

        /// distance from starting location
        dist: f64,

        /// x offset from starting position
        dx: f64,

        /// y offset from starting position
        dy: f64,

        /// star coordinates
        pos: [f64; 2],

        /// step number
        step: i32,

        #[serde(rename = "State")]
        /// calibration status message
        state: String,
    },

    /// Calibration completed successfully
    CalibrationComplete {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Mount")]
        /// name of the mount that was calibrated
        mount: String,
    },

    /// A star has been selected
    StarSelected {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "X")]
        /// lock position X-coordinate
        x: f64,

        #[serde(rename = "Y")]
        /// lock position Y-coordinate
        y: f64,
    },

    /// Guiding begins
    StartGuiding {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// Guiding has been paused
    Paused {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// Calibration begins
    StartCalibration {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Mount")]
        /// the name of the mount being calibrated
        mount: String,
    },

    /// Current application state
    AppState {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "State")]
        /// the current state of PHD
        state: Phd2AppState,
    },

    /// Calibration failed
    CalibrationFailed {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Reason")]
        /// an error message string
        reason: String,
    },

    /// Calibration data has been flipped
    CalibrationDataFlipped {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Mount")]
        /// the name of the mount
        mount: String,
    },

    /// The lock position shift is active and the lock position
    /// has shifted to the edge of the field of view
    LockPositionShiftLimitReached {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// Sent for each exposure frame while looping exposures
    LoopingExposures {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Frame")]
        /// the exposure frame number; starts at 1 each time looping starts
        frame: usize,
    },

    /// Looping exposures has stopped
    LoopingExposuresStopped {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// Sent when settling begins after a `dither` or `guide` method invocation
    SettleBegin {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// Sent for each exposure frame after a `dither` or `guide`
    /// method invocation until guiding has settled
    Settling {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Distance")]
        /// the current distance between the guide star and lock position
        distance: f64,

        #[serde(rename = "Time")]
        /// the elapsed time that the distance has been below the settling tolerance
        /// distance (the `pixels` attribute of the `SETTLE` parameter)
        time: f64,

        #[serde(rename = "SettleTime")]
        /// the requested settle time (the `time` attribute of the `SETTLE` parameter)
        settle_time: f64,

        #[serde(rename = "StarLocked")]
        /// true if the guide star was found in the current camera frame,
        /// false if the guide star was lost
        star_locked: bool,
    },

    /// Sent after a `dither` or `guide` method invocation indicating whether
    /// settling was achieved, or if the guider failed to settle before the time
    /// limit was reached, or if some other error occurred preventing `guide` or
    /// `dither` to complete and settle
    SettleDone {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Status")]
        /// 0 if settling succeeded, non-zero if it failed
        status: i32,

        #[serde(rename = "Error")]
        /// a description of the reason why the `guide` or `dither` command
        /// failed to complete and settle
        error: String,

        #[serde(rename = "TotalFrames")]
        /// the number of camera frames while settling
        total_frames: usize,

        #[serde(rename = "DroppedFrames")]
        /// the number of dropped camera frames (guide star not found) while settling
        dropped_frames: usize,
    },

    /// A frame has been dropped due to the star being lost
    StarLost {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Frame")]
        /// frame number
        frame: usize,

        #[serde(rename = "Time")]
        /// time since guiding started, seconds
        time: f64,

        #[serde(rename = "StarMass")]
        /// star mass value
        star_mass: f64,

        #[serde(rename = "SNR")]
        /// star SNR value
        snr: f64,

        #[serde(rename = "AvgDist")]
        /// a smoothed average of the guide distance in pixels
        /// (equivalent to value returned by socket server MSG\_REQDIST)
        avg_dist: f64,

        #[serde(rename = "ErrorCode")]
        /// error code
        error_code: i32,

        #[serde(rename = "Status")]
        /// error message
        status: String,
    },

    /// Guiding has stopped
    GuidingStopped {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// PHD has been resumed after having been paused
    Resumed {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// This event corresponds to a line in the PHD Guide Log.
    /// The event is sent for each frame while guiding
    GuideStep {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Frame")]
        /// The frame number; starts at 1 each time guiding starts
        frame: usize,

        #[serde(rename = "Time")]
        /// the time in seconds, including fractional seconds, since guiding started
        time: f64,

        #[serde(rename = "Mount")]
        /// the name of the mount
        mount: String,

        /// the X-offset in pixels
        dx: f64,

        /// the Y-offset in pixels
        dy: f64,

        #[serde(rename = "RADistanceRaw")]
        /// the RA distance in pixels of the guide offset vector
        ra_distance_raw: f64,

        #[serde(rename = "DECDistanceRaw")]
        /// the Dec distance in pixels of the guide offset vector
        dec_distance_raw: f64,

        #[serde(rename = "RADistanceGuide")]
        /// the guide algorithm-modified RA distance in pixels of the guide offset vector
        ra_distance_guide: f64,

        #[serde(rename = "DECDistanceGuide")]
        /// the guide algorithm-modified Dec distance in pixels of the guide offset vector
        dec_distance_guide: f64,

        #[serde(rename = "RADuration")]
        /// the RA guide pulse duration in milliseconds
        ra_duration: Option<f64>,

        #[serde(rename = "RADirection")]
        /// "East" or "West"
        ra_direction: Option<RADirection>,

        #[serde(rename = "DECDuration")]
        /// the Dec guide pulse duration in milliseconds
        dec_duration: Option<f64>,

        #[serde(rename = "DECDirection")]
        /// "South" or "North"
        dec_direction: Option<DECDirection>,

        #[serde(rename = "StarMass")]
        /// the Star Mass value of the guide star
        star_mass: f64,

        #[serde(rename = "SNR")]
        /// the computed Signal-to-noise ratio of the guide star
        snr: f64,

        #[serde(rename = "HFD")]
        /// the guide star half-flux diameter (HFD) in pixels
        hfd: f64,

        #[serde(rename = "AvgDist")]
        /// a smoothed average of the guide distance in pixels
        /// (equivalent to value returned by socket server MSG\_REQDIST
        avg_dist: f64,

        #[serde(rename = "RALimited")]
        /// true if step was limited by the Max RA setting (attribute omitted if step was not limited)
        ra_limited: Option<bool>,

        #[serde(rename = "DecLimited")]
        /// true if step was limited by the Max Dec setting (attribute omitted if step was not limited)
        dec_limited: Option<bool>,

        #[serde(rename = "ErrorCode")]
        /// the star finder error code
        error_code: Option<i32>,
    },

    /// The lock position has been dithered
    GuidingDithered {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        /// the dither X-offset in pixels
        dx: f64,

        /// the dither Y-offset in pixels
        dy: f64,
    },

    /// The lock position has been lost
    LockPositionLost {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },

    /// An alert message was displayed in PHD2
    Alert {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Msg")]
        /// the text of the alert message
        msg: String,

        #[serde(rename = "Type")]
        alert_type: AlertType,
    },

    /// A guiding parameter has been changed
    GuideParamChange {
        #[serde(flatten)]
        common: Phd2EventCommonData,

        #[serde(rename = "Name")]
        /// the name of the parameter that changed
        name: String,

        #[serde(rename = "Value")]
        /// the new value of the parameter
        value: String,
    },

    /// Notification sent when any settings are changed --
    /// allows a client to keep in sync with PHD2 configuration
    /// settings by exporting settings only when required
    ConfigurationChange {
        #[serde(flatten)]
        common: Phd2EventCommonData,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// All messages contain the following attributes in common
pub struct Phd2EventCommonData {
    #[serde(rename = "Timestamp")]
    /// the timesamp of the event in seconds from the epoch, including fractional seconds
    timestamp: f64,

    #[serde(rename = "Host")]
    /// the hostname of the machine running PHD
    host: String,

    #[serde(rename = "Inst")]
    /// the PHD instance number (1-based)
    inst: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Phd2AppState {
    Stopped,
    Selected,
    Calibrating,
    Guiding,
    LostLock,
    Paused,
    Looping
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RADirection {
    East,
    West
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DECDirection {
    South,
    North
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AlertType {
    #[serde(rename = "info")]
    Info,

    #[serde(rename = "question")]
    Question,

    #[serde(rename = "warning")]
    Warning,

    #[serde(rename = "error")]
    Error
}

#[derive(Debug, Clone)]
pub enum Phd2Event {
    Started,
    Stopped,
    Connected,
    Disconnected,
    Object(Phd2IncomingObject),
}

type Phd2EventFun = dyn Fn(Phd2Event) + 'static + Send + Sync;
type EventHandlers = Arc<RwLock<Vec<Box<Phd2EventFun>>>>;

pub struct Phd2Conn {
    exit_flag:        Arc<AtomicBool>,
    main_thread:      Arc<Mutex<Option<JoinHandle<()>>>>,
    read_tcp_stream:  Arc<Mutex<Option<TcpStream>>>,
    write_tcp_stream: Arc<Mutex<Option<BufWriter<TcpStream>>>>,
    event_handlers:   EventHandlers,
    cmd_id:           Arc<AtomicUsize>,
}

impl Phd2Conn {
    pub fn new() -> Self {
        Self {
            exit_flag:        Arc::new(AtomicBool::new(false)),
            main_thread:      Arc::new(Mutex::new(None)),
            read_tcp_stream:  Arc::new(Mutex::new(None)),
            write_tcp_stream: Arc::new(Mutex::new(None)),
            event_handlers:   Arc::new(RwLock::new(Vec::new())),
            cmd_id:           Arc::new(AtomicUsize::new(1)),
        }
    }

    pub fn connect_event_handler(&self, fun: impl Fn(Phd2Event) + 'static + Send + Sync) {
        self.event_handlers.write().unwrap().push(Box::new(fun));
    }

    pub fn work(&self, host: &str, port: u16) -> anyhow::Result<()> {
        log::debug!("Phd2Conn::work");

        let mut self_main_thread = self.main_thread.lock().unwrap();
        if self_main_thread.is_some() {
            anyhow::bail!("Already working");
        }

        self.exit_flag.store(false, Ordering::Relaxed);

        let host_and_port_string = format!("{}:{}", host, port);
        let exit_flag = Arc::clone(&self.exit_flag);
        let self_send_stream = Arc::clone(&self.write_tcp_stream);
        let read_tmp_stream = Arc::clone(&self.read_tcp_stream);
        let event_handlers = Arc::clone(&self.event_handlers);
        let host = host.to_string();

        let main_thread = spawn(move || {
            log::debug!("Begin PHD2 stream");
            Self::notify_event(&event_handlers, Phd2Event::Started);

            // Main loop
            'main_loop: loop {
                // Connecting...
                let read_stream = loop {
                    let conn_result = TcpStream::connect(&host_and_port_string);
                    match conn_result {
                        Ok(conn_result) =>
                            break conn_result,
                        Err(_) => {
                            for _ in 0..10 { // wait 1000 ms before next try to connect
                                if exit_flag.load(Ordering::Relaxed) {
                                    break 'main_loop;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            continue;
                        }
                    }
                };
                let Ok(send_stream) = read_stream.try_clone() else {
                    break;
                };

                Self::notify_event(&event_handlers, Phd2Event::Connected);
                log::debug!("Connected to PHD2 at {}:{}", host, port);

                *self_send_stream.lock().unwrap() = Some(BufWriter::new(send_stream));
                *read_tmp_stream.lock().unwrap() = Some(read_stream.try_clone().unwrap()); // ??? too dangerous

                // Reading PHD2's jsons
                let mut buffer = Vec::new();
                let mut buffered_stream = BufReader::new(read_stream);
                loop {
                    let mut byte = [0u8];
                    let read = match buffered_stream.read(&mut byte) {
                        Ok(read) => read,
                        Err(err) => {
                            log::debug!("PHD2 read_stream.read returned {}", err.to_string());
                            break;
                        }
                    };
                    if read == 0 { break; }
                    buffer.push(byte[0]);
                    if byte[0] == b'\n' {
                        let Ok(js_str) = std::str::from_utf8(&buffer) else {
                            continue;
                        };
                        let res = Self::process_incoming_json(&event_handlers, js_str);
                        if let Err(err) = res {
                            println!("{}", js_str);
                            println!("{}", err.to_string());
                            log::error!("Error during processing PHD2 json: {}", err.to_string());
                        }
                        buffer.clear();
                    }
                }

                Self::notify_event(&event_handlers, Phd2Event::Disconnected);

                let exit_flag = exit_flag.load(Ordering::Relaxed);
                log::debug!("Exited from reading PHD2 stream, exit_flag = {}", exit_flag);

                *self_send_stream.lock().unwrap() = None;
                *read_tmp_stream.lock().unwrap() = None;

                if exit_flag { break; }
            }
            log::debug!("Exit read PHD2 stream");
            Self::notify_event(&event_handlers, Phd2Event::Stopped);
        });
        *self_main_thread = Some(main_thread);
        Ok(())
    }

    pub fn stop(&self) -> anyhow::Result<()> {
        log::debug!("Phd2Conn::stop");

        // Set stop flag to true
        self.exit_flag.store(true, Ordering::Relaxed);

        // Shutdown TCP stream
        let mut self_send_stream = self.read_tcp_stream.lock().unwrap();
        if let Some(send_stream) = self_send_stream.take() {
            _ = send_stream.shutdown(Shutdown::Both);
        }
        drop(self_send_stream);

        // Wait while main thread finished
        let mut self_main_thread = self.main_thread.lock().unwrap();
        if let Some(main_thread) = self_main_thread.take() {
            _ = main_thread.join();
        } else {
            anyhow::bail!("Not working");
        }
        drop(self_main_thread);

        Ok(())
    }

    pub fn is_working(&self) -> bool {
        self.main_thread.lock().unwrap().is_some()
    }

    pub fn command_pause(&self, pause: bool, full: bool) -> anyhow::Result<()> {
        log::debug!("Phd2Conn::command_pause, pause = {}, full = {}", pause, full);
        let full_flag = if full { Some("full") } else { None };
        let cmd = Command::SetPaused {
            params: (pause, full_flag),
            id:     self.cmd_id.fetch_add(1, Ordering::Relaxed),
        };
        self.send_command(&serde_json::to_string(&cmd)?)?;
        Ok(())
    }

    fn notify_event(event_handlers: &EventHandlers, event: Phd2Event) {
        let event_handlers = event_handlers.read().unwrap();
        for handler in &*event_handlers {
            handler(event.clone());
        }
    }

    fn process_incoming_json(event_handlers: &EventHandlers, js_str: &str)  -> anyhow::Result<()> {
        let js_obj: Phd2IncomingObject = serde_json::from_str(js_str)?;
        Self::notify_event(event_handlers, Phd2Event::Object(js_obj));
        Ok(())
    }

    fn send_command(&self, command: &str) -> anyhow::Result<()> {
        log::debug!("Phd2Conn::send_command, command = {}", command);
        let mut self_send_stream = self.write_tcp_stream.lock().unwrap();
        if let Some(send_stream) = &mut *self_send_stream {
            send_stream.write_all(command.as_bytes())?;
            send_stream.write_all(b"\r\n")?;
            send_stream.flush()?;
            Ok(())
        } else {
            anyhow::bail!("Is not connected to PHD2 now");
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "method")]
enum Command {
    #[serde(rename = "set_paused")]
    SetPaused {
        params: (bool/*paused*/, Option<&'static str>/*`full` flag*/),
        id: usize,
    }
}

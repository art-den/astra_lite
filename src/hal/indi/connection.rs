#![allow(dead_code)]

use std::io::{prelude::*, BufWriter, Cursor};
use std::net::TcpStream;

use std::process::{Command, Child, Stdio};
use std::sync::{MutexGuard, atomic::*};
use std::sync::{Mutex, Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use bitflags::bitflags;
use chrono::prelude::*;

use super::{xml_reader::*, error::*, xml_helper::*, events::*, property::*, device::*};


#[derive(Clone)]
pub struct ConnSettings {
    pub remote: bool,
    pub host: String,
    pub server_exe: String,
    pub drivers: Vec<String>,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            remote: false,
            host: "localhost".to_string(),
            server_exe: "indiserver".to_string(),
            drivers: Vec::new(),
        }
    }
}

enum XmlSenderItem {
    Xml(String),
    Exit
}

pub enum EventSenderEvent {
    Mess(Event),
    Exit,
}

#[derive(Debug)]
enum Stream {
    Tcp(TcpStream),
    #[cfg(target_os = "linux")]
    Unix(std::os::unix::net::UnixStream),
}

impl Stream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
            #[cfg(target_os = "linux")]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
        }
    }

    fn as_read(&mut self) -> &mut dyn std::io::Read {
        match self {
            Self::Tcp(stream) => stream,
            #[cfg(target_os = "linux")]
            Self::Unix(stream) => stream,
        }
    }

    fn as_write_box(self) -> Box<dyn std::io::Write> {
        match self {
            Self::Tcp(stream) => Box::new(stream),
            #[cfg(target_os = "linux")]
            Self::Unix(stream) => Box::new(stream),
        }
    }

    fn try_clone(&self) -> std::io::Result<Stream> {
        Ok(match self {
            Self::Tcp(stream) => Stream::Tcp(stream.try_clone()?),
            #[cfg(target_os = "linux")]
            Self::Unix(stream) => Stream::Unix(stream.try_clone()?),
        })
    }

    fn shutdown(&self, how: std::net::Shutdown) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown(how),
            #[cfg(target_os = "linux")]
            Self::Unix(stream) => stream.shutdown(how),
        }
    }
}

struct ActiveConnData {
    indiserver:     Option<Child>,
    stream:         Stream,
    xml_sender:     XmlSender,
    events_thread:  JoinHandle<()>,
    read_thread:    JoinHandle<()>,
    write_thread:   JoinHandle<()>,
    events_sender:  std::sync::mpsc::Sender<EventSenderEvent>,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error(String)
}

#[derive(Debug)]
pub struct DriverInfo {
    pub interface: DriverInterface,
    pub driver:    Arc<String>,
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct DriverInterface: u32 {
        const GENERAL       = 0;
        const TELESCOPE     = (1 << 0);
        const CCD           = (1 << 1);
        const GUIDER        = (1 << 2);
        const FOCUSER       = (1 << 3);
        const FILTER        = (1 << 4);
        const DOME          = (1 << 5);
        const GPS           = (1 << 6);
        const WEATHER       = (1 << 7);
        const AO            = (1 << 8);
        const DUSTCAP       = (1 << 9);
        const LIGHTBOX      = (1 << 10);
        const DETECTOR      = (1 << 11);
        const ROTATOR       = (1 << 12);
        const SPECTROGRAPH  = (1 << 13);
        const CORRELATOR    = (1 << 14);
        const AUX           = (1 << 15);
    }
}

#[derive(Debug, Clone)]
pub struct ExportDevice {
    pub name:      Arc<String>,
    pub interface: DriverInterface,
    pub driver:    Arc<String>,
    pub connected: bool,
}

pub enum FrameType {
    Light,
    Flat,
    Dark,
    Bias,
}

pub enum CaptureFormat {
    Rgb,
    Raw,
}

pub enum BinningMode {
    Add,
    Avg,
}

pub enum AfterCoordSetAction {
    Track,
    Slew,
    Sync,
}

pub struct Connection {
    data:            Arc<Mutex<Option<ActiveConnData>>>,
    state:           Arc<Mutex<ConnState>>,
    devices:         Arc<Mutex<Devices>>,
    event_handlers:  Arc<EventHandlers>,
    drivers_started: AtomicBool,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(
                None
            )),
            state: Arc::new(Mutex::new(
                ConnState::Disconnected
            )),
            devices: Arc::new(Mutex::new(
                Devices::new()
            )),
            event_handlers: Arc::new(
                EventHandlers::new()
            ),
            drivers_started: AtomicBool::new(false),
        }
    }

    pub fn connect_event_handler(
        &self,
        fun: impl Fn(Event) + Send + 'static
    ) -> EventHandlerId {
        self.event_handlers.connect(fun)
    }

    pub fn disconnect_event_handler(&self, event_handlers: EventHandlerId) {
        self.event_handlers.disconnect(event_handlers);
    }

    pub fn disconnect_all_event_handlers(&self) {
        self.event_handlers.disconnect_all();
    }

    fn start_indi_server(
        exe:     &str,
        drivers: &[String],
    ) -> eyre::Result<Child> {
        // Start indiserver process
        let mut child = Command::new(exe)
            .args(drivers)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        // Wait 1 second and check if it's alive
        std::thread::sleep(Duration::from_millis(1000));
        if let Ok(Some(status)) = child.try_wait() {
            // kill zombie
            _ = child.kill();
            _ = child.wait();
            // read stderr of process and return error information
            let mut stderr_str = String::new();
            let stderr_ok = child.stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr_str).is_ok();
            if stderr_ok {
                let stderr_lines: Vec<_> = stderr_str
                    .split("\n")
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.ends_with("good bye"))
                    .collect();
                if !stderr_lines.is_empty() {
                    let mut err_text = *stderr_lines.last().unwrap();
                    if let Some(space_pos) = err_text.find(" ") {
                        err_text = &err_text[space_pos..];
                    }
                    eyre::bail!(
                        "Process `{}` terminated with code `{}` and text `{}`",
                        exe, status.code().unwrap_or(0), err_text
                    );
                }
            }
            eyre::bail!(
                "Process `{}` terminated with code `{}`",
                exe, status.code().unwrap_or(0)
            );
        }
        Ok(child)
    }

    fn create_tcp_stream(settings: &ConnSettings) -> Result<TcpStream> {
        use std::net::ToSocketAddrs;

        let mut addr = if settings.remote {
            settings.host.clone()
        } else {
            "localhost".to_string()
        };
        if !addr.contains(":") { addr += ":7624"; }

        // Resolve host into IP addresses
        let sock_addrs = addr.to_socket_addrs()?;

        // Try to connect INDI server 5 times in 1 second
        let mut last_conn_try_result: Option<std::io::Error> = None;
        for addr in sock_addrs {
            for _ in 0..5 {
                let conn_try_res = TcpStream::connect_timeout(
                    &addr,
                    Duration::from_millis(1000)
                );
                match conn_try_res {
                    Ok(res) => return Ok(res),
                    Err(err) => last_conn_try_result = Some(err),
                }
            }
        }
        Err(Error::IO(last_conn_try_result.expect("last_conn_try_result")))
    }

    #[cfg(target_os = "linux")]
    fn create_unix_stream() -> Result<std::os::unix::net::UnixStream> {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixStream};

        let addr = SocketAddr::from_abstract_name("/tmp/indiserver")?;
        // Try to connect INDI server 5 times in 1 second
        let mut last_conn_try_result: Option<std::io::Error> = None;
        for _ in 0..5 {
            let conn_try_res = UnixStream::connect_addr(&addr);
            match conn_try_res {
                Ok(res) => return Ok(res),
                Err(err) => last_conn_try_result = Some(err),
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        Err(Error::IO(last_conn_try_result.expect("last_conn_try_result")))
    }

    #[cfg(target_os = "linux")]
    fn create_stream(settings: &ConnSettings) -> Result<Stream> {
        let unix_stream =
            !settings.remote ||
            settings.host == "localhost" ||
            settings.host.starts_with("127.");
        let result = if unix_stream {
            Stream::Unix(Self::create_unix_stream()?)
        } else {
            Stream::Tcp(Self::create_tcp_stream(settings)?)
        };
        Ok(result)
    }

    #[cfg(not(target_os = "linux"))]
    fn create_stream(settings: &ConnSettings) -> Result<Stream> {
        Ok(Stream::Tcp(Self::create_tcp_stream(settings)?))
    }

    pub fn connect(self: &Arc<Self>, settings: &ConnSettings) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        match *state {
            ConnState::Connecting =>
                return Err(Error::WrongSequence("Already connecting".to_string())),
            ConnState::Connected =>
                return Err(Error::WrongSequence("Already connected".to_string())),
            _ => {},
        }
        Self::set_new_conn_state(
            ConnState::Connecting,
            &mut state,
            &self.event_handlers
        );
        drop(state);
        let settings = settings.clone();
        let self_ = Arc::clone(self);
        std::thread::spawn(move || {
            // Start indi drivers
            let mut indiserver = if !settings.remote {
                let start_result = Self::start_indi_server(
                    &settings.server_exe,
                    &settings.drivers,
                );
                match start_result {
                    Ok(child) => Some(child),
                    Err(err) => {
                        Self::set_new_conn_state(
                            ConnState::Error(err.to_string()),
                            &mut self_.state.lock().unwrap(),
                            &self_.event_handlers
                        );
                        return;
                    }
                }
            } else {
                None
            };

            let Ok(stream) = Self::create_stream(&settings) else {
                // Failed to connect. Stop INDI server and exit
                if let Some(indiserver) = &mut indiserver {
                    _ = indiserver.kill();
                    _ = indiserver.wait();
                }
                Self::set_new_conn_state(
                    ConnState::Error(format!("Can't connect to {}", settings.host)),
                    &mut self_.state.lock().unwrap(),
                    &self_.event_handlers
                );
                return;
            };

            // Subscribers event thread for XML receiver
            let (events_sender, events_receiver) = mpsc::channel();
            let events_thread = {
                let self_ = Arc::clone(&self_);
                std::thread::spawn(move || {
                    log::info!("Enter events_thread");
                    while let Ok(event) = events_receiver.recv() {
                        match event {
                            EventSenderEvent::Mess(event) => {
                                if let Event::ConnChange(state) = &event
                                && *state == ConnState::Disconnected
                                && *self_.state.lock().unwrap() == ConnState::Connected {
                                    self_.event_handlers.send(Event::ConnectionLost);
                                    std::thread::spawn(move || {
                                        _ = self_.disconnect_and_wait();
                                    });
                                    break;
                                }
                                self_.event_handlers.send(event);
                            }
                            EventSenderEvent::Exit => break,
                        }
                    }
                    log::info!("Exit events_thread");
                })
            };

            // Start XML receiver thread
            let (xml_sender, xml_to_send) = mpsc::channel();
            let read_thread = {
                let events_sender_clone = events_sender.clone();
                let xml_sender = xml_sender.clone();
                let stream = stream.try_clone().unwrap();
                let self_ = Arc::clone(&self_);
                std::thread::spawn(move || {
                    log::info!("Enter read_thread");
                    let mut receiver = XmlReceiver::new(
                        &self_.state,
                        &self_.devices,
                        stream,
                        XmlSender { xml_sender },
                    );
                    receiver.main(events_sender_clone);
                    log::info!("Exit read_thread");
                })
            };

            // Start XML sender thread
            let write_thread = {
                let stream = stream.try_clone().unwrap();
                std::thread::spawn(move || {
                    log::info!("Enter write_thread");
                    XmlSender::main(xml_to_send, stream);
                    log::info!("Exit write_thread");
                })
            };

            // take indiserver stderr
            let indiserver_stderr = indiserver
                .as_mut()
                .and_then(|v| v.stderr.take());

            // Assign active connection data
            *self_.data.lock().unwrap() = Some(ActiveConnData{
                indiserver,
                stream,
                xml_sender: XmlSender { xml_sender },
                events_thread,
                read_thread,
                write_thread,
                events_sender,
            });

            self_.drivers_started.store(!settings.remote, Ordering::Relaxed);

            // Read from indiserver's stderr and inform event handlers
            if let Some(mut indiserver_stderr) = indiserver_stderr {
                let mut stderr_data = Vec::new();
                let mut buffer = [0_u8; 256];
                while let Ok(read) = indiserver_stderr.read(&mut buffer) {
                    stderr_data.extend_from_slice(&buffer[..read]);
                    if read == 0 { break; }
                    // TODO: parse error text and inform subscribers
                }
            }
        });

        Ok(())
    }

    pub fn is_drivers_started(&self) -> bool {
        self.drivers_started.load(Ordering::Relaxed)
    }

    fn set_new_conn_state(
        new_state:      ConnState,
        state:          &mut ConnState,
        event_handlers: &EventHandlers
    ) {
        if new_state == *state { return; }
        *state = new_state;
        event_handlers.send(Event::ConnChange(state.clone()));
    }

    pub fn disconnect_and_wait(&self) -> Result<()> {
        Self::set_new_conn_state(
            ConnState::Disconnecting,
            &mut self.state.lock().unwrap(),
            &self.event_handlers
        );
        let mut data = self.data.lock().unwrap();
        if let Some(conn) = data.take() {
            drop(data);

            // Send exit command to xml_sender queue
            conn.xml_sender.send_exit_to_thread();

            // Shut down network connection
            _ = conn.stream.shutdown(std::net::Shutdown::Both);

            // Waiting for xml_sender and xml_receiver threads to terminate
            _ = conn.read_thread.join();
            _ = conn.write_thread.join();

            // Send "exit" message to events_thread
            _ = conn.events_sender.send(EventSenderEvent::Exit);

            // Waiting for events thread to terminate
            _ = conn.events_thread.join();

            // Killing indiserver
            if let Some(mut indiserver) = conn.indiserver {
                _ = indiserver.kill();
                _ = indiserver.wait();
            }

            // Clear devices properties
            self.devices.lock().unwrap().clear();

            // Set new "disconnected" state
            Self::set_new_conn_state(
                ConnState::Disconnected,
                &mut self.state.lock().unwrap(),
                &self.event_handlers
            );
        } else {
            return Err(Error::WrongSequence("Not connected".into()));
        }
        Ok(())
    }

    pub fn state(&self) -> ConnState {
        self.state.lock().unwrap().clone()
    }

    pub fn get_devices_list(&self) -> Vec<ExportDevice> {
        let devices = self.devices.lock().unwrap();
        devices.get_list_iter().collect()
    }

    pub fn device_exists(&self, device_name: &str) -> bool {
        let devices = self.devices.lock().unwrap();
        devices.find_by_name_opt(device_name).is_some()
    }

    pub fn get_devices_list_by_interface(&self, iface: DriverInterface) -> Vec<ExportDevice> {
        let devices = self.devices.lock().unwrap();
        devices
            .get_list_iter()
            .filter(|device| device.interface.intersects(iface))
            .collect()
    }

    pub fn get_driver_info(&self, device_name: &str) -> Result<DriverInfo> {
        let devices = self.devices.lock().unwrap();
        devices.get_driver_info(device_name)
    }

    pub fn get_properties_list(
        &self,
        changed_after: Option<u64>,
    ) -> (Vec<ExportDevice>, Vec<Property>) {
        let devices = self.devices.lock().unwrap();

        let devices_list = devices.get_list_iter().collect();
        let properties_list = devices.get_properties_list(changed_after);
        (devices_list, properties_list)
    }

    pub fn property_exists(
        &self,
        device_name: &str,
        prop_name: &str,
        elem_name: Option<&str>
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        devices.property_exists(device_name, prop_name, elem_name)
    }

    pub fn get_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        devices.get_switch_property(device_name, prop_name, elem_name)
    }

    pub fn get_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let elem = devices.get_num_property(device_name, prop_name, elem_name)?;
        Ok(elem.clone())
    }

    pub fn get_num_property_value(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_num_property(device_name, prop_name, elem_name)?;
        Ok(property.value)
    }

    pub fn get_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<Arc<String>> {
        let devices = self.devices.lock().unwrap();
        devices.get_text_property(device_name, prop_name, elem_name)
    }

    fn with_conn_data_or_err(
        &self,
        fun: impl FnOnce(&ActiveConnData) -> Result<()>
    ) -> Result<()> {
        if let Some(ref conn_data) = *self.data.lock().unwrap() {
            fun(conn_data)
        } else {
            Err(Error::WrongSequence("Not connected".into()))
        }
    }

    pub fn command_get_properties(
        &self,
        device_name: Option<&str>,
        prop_name:   Option<&str>
    ) -> Result<()> {
        self.with_conn_data_or_err(move |data| {
            data.xml_sender.command_get_properties_impl(device_name, prop_name)
        })?;
        Ok(())
    }

    pub fn command_enable_blob(
        &self,
        device_name: &str,
        prop_name:   Option<&str>,
        mode:        BlobEnable
    ) -> Result<()> {
        self.with_conn_data_or_err(move |data| {
            data.xml_sender.command_enable_blob(device_name, prop_name, mode)
        })?;
        Ok(())
    }

    pub fn command_enable_device(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem = if enable {
            "CONNECT"
        } else {
            "DISCONNECT"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CONNECTION",
            &[(elem, true)],
        )?;
        Ok(())
    }

    pub fn is_device_enabled(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CONNECTION",
            "CONNECT"
        )
    }

    pub fn command_enable_all_devices(
        &self,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let dev_list = devices.get_list_iter().collect::<Vec<_>>();
        drop(devices);
        for dev in &dev_list {
            self.command_enable_device(
                &dev.name,
                enable,
                force_set,
                timeout_ms
            )?;
        }
        Ok(())
    }

    pub fn command_set_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, &str)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        self.devices.lock().unwrap().check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Text),
            |index| elements[index].0,
            "Text",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_text_property(
                device_name,
                prop_name,
                elements
            )
        })?;
        Ok(())
    }

    pub fn command_set_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        self.devices.lock().unwrap().check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Switch(_)),
            |index| elements[index].0,
            "Switch",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_switch_property(
                device_name,
                prop_name,
                elements
            )
        })?;
        Ok(())
    }

    pub fn check_switch_property_is_eq(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        for (elem_name, expected_value) in elements {
            let prop_value = devices.get_switch_property(
                device_name,
                prop_name,
                elem_name
            )?;
            if prop_value != *expected_value {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn command_set_switch_property_and_wait(
        &self,
        force_set:   bool,
        timeout_ms:  Option<u64>,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)],
    ) -> Result<()> {
        if !force_set
        && self.check_switch_property_is_eq(device_name, prop_name, elements)? {
            return Ok(());
        }
        self.command_set_switch_property(
            device_name,
            prop_name,
            elements
        )?;
        if let Some(mut timeout_ms) = timeout_ms {
            const TIME_QUANT_MS: u64 = 100;
            loop {
                let prop_eq = self.check_switch_property_is_eq(
                    device_name,
                    prop_name,
                    elements
                )?;
                if prop_eq || timeout_ms < TIME_QUANT_MS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(TIME_QUANT_MS));
                timeout_ms -= TIME_QUANT_MS;
                log::debug!("Waiting to set {}.{} property...", device_name, prop_name);
            }
        }
        Ok(())
    }

    fn command_set_num_property_impl(
        &self,
        devices:     &mut MutexGuard<Devices>,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        devices.check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Num),
            |index| elements[index].0,
            "Num",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_num_property(
                device_name,
                prop_name,
                elements
            )?;
            devices.mark_property_as_busy(
                device_name,
                prop_name,
                &data.events_sender
            )?;
            Ok(())
        })?;
        Ok(())
    }


    pub fn command_set_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<()> {
        let mut devices = self.devices.lock().unwrap();
        self.command_set_num_property_impl(&mut devices, device_name, prop_name, elements)?;
        Ok(())
    }

    fn f64_prop_values_equal(value1: f64, value2: f64) -> bool {
        if value1.is_nan() && value2.is_nan() {
            return true;
        }
        if value1.is_nan() != value2.is_nan() {
            return false;
        }
        if value1 == value2 {
            return true;
        }
        let avg = (value1.abs() + value2.abs()) / 2.0;
        let min_diff = avg / 1e6;
        f64::abs(value1 - value2) < min_diff
    }

    fn check_num_property_is_eq(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        for (elem_name, expected_value) in elements {
            let prop = devices.get_num_property(
                device_name,
                prop_name,
                elem_name
            )?;
            if !Self::f64_prop_values_equal(prop.value, *expected_value) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn command_set_num_property_and_wait(
        &self,
        force_set:   bool,
        timeout_ms:  Option<u64>,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)],
    ) -> Result<()> {
        if !force_set
        && self.check_num_property_is_eq(device_name, prop_name, elements)? {
            return Ok(());
        }
        self.command_set_num_property(
            device_name,
            prop_name,
            elements
        )?;
        if let Some(mut timeout_ms) = timeout_ms {
            const TIME_QUANT_MS: u64 = 100;
            loop {
                let prop_eq = self.check_num_property_is_eq(
                    device_name,
                    prop_name,
                    elements
                )?;
                if prop_eq || timeout_ms < TIME_QUANT_MS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(TIME_QUANT_MS));
                timeout_ms -= TIME_QUANT_MS;
                log::debug!("Waiting to set {}.{} property...", device_name, prop_name);
            }
        }
        Ok(())
    }

    fn is_device_support_any_of_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let result = devices.existing_prop_name_opt(
            device,
            props
        );
        Ok(result.is_some())
    }

    pub fn device_get_prop_elem(
        &self,
        device_name: &str,
        props:       PropsNamePairs
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let value = devices.get_num_property(
            device_name,
            prop_name, elem_name
        )?;
        Ok(value.clone())
    }

    pub fn device_set_any_of_num_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
        value:       f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        drop(devices);
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name,
            &[(elem_name, value)]
        )
    }

    pub fn set_any_of_switch_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
        value:       bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name,
            &[(elem_name, value)]
        )
    }

    pub fn device_get_num_prop(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let property = devices.get_num_property(
            device_name,
            prop_name,
            elem_name
        )?;
        Ok(property.clone())
    }

    pub fn device_get_num_prop_value(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let property = devices.get_num_property(
            device_name,
            prop_name,
            elem_name
        )?;
        Ok(property.value)
    }

    pub fn device_get_any_of_switch_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        devices.get_switch_property(
            device_name,
            prop_name,
            elem_name
        )
    }

    // Crash device

    pub fn device_is_simu_crash_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_DEVICE_CRASH
        )
    }

    pub fn device_crash(
        &self,
        device_name: &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.set_any_of_switch_props(
            device_name,
            PROP_DEVICE_CRASH,
            true,
            force_set,
            timeout_ms
        )
    }

    // Device polling period

    pub fn device_is_polling_period_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(device_name, "POLLING_PERIOD", None)
    }

    pub fn device_get_polling_period(
        &self,
        device_name: &str,
    ) -> Result<usize> {
        let result = self.get_num_property(
            device_name,
            "POLLING_PERIOD",
            "PERIOD_MS"
        )?;
        Ok(result.value as usize)
    }

    pub fn device_set_polling_period(
        &self,
        device_name:    &str,
        polling_period: usize,
        force_set:      bool,
        timeout_ms:     Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "POLLING_PERIOD",
            &[("PERIOD_MS", polling_period as f64)]
        )
    }

    // Fast toggle capability

    pub fn camera_is_fast_toggle_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_FAST_TOGGLE",
            None
        )
    }

    pub fn camera_enable_fast_toggle(
        &self,
        device_name: &str,
        enabled:     bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_FAST_TOGGLE", &[
            ("INDI_ENABLED", enabled),
            ("INDI_DISABLED", !enabled)
        ])
    }

    pub fn camera_is_fast_toggle_enabled(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CCD_FAST_TOGGLE",
            "INDI_ENABLED"
        )
    }

    pub fn camera_get_fast_frames_count_prop_info(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_num_property(
            device_name,
            "CCD_FAST_COUNT",
            "FRAMES"
        )?;
        Ok(property.clone())
    }

    pub fn camera_set_fast_frames_count(
        &self,
        device_name: &str,
        frames:      usize,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_FAST_COUNT",
            &[("FRAMES", frames as f64)]
        )
    }

    // Exposure


    pub fn camera_get_exposure_property(
        &self,
        device_name: &str,
        ccd:         CamCcd
    ) -> Result<(Property, PropElement)> {
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        let devices = self.devices.lock().unwrap();
        self.get_property_and_element(&devices, device_name, prop_name, prop_elem)
    }

    pub fn camera_get_exposure_prop_value(
        &self,
        device_name: &str,
        ccd:         CamCcd
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        let property = devices.get_num_property(
            device_name,
            prop_name,
            prop_elem,
        )?;
        Ok(property.clone())
    }

    pub fn camera_is_exposure_property(
        prop_name: &str,
        elem_name: &str,
        ccd:       CamCcd
    ) -> bool {
        let (name, elem) = Self::exposure_prop_name(ccd);
        prop_name == name && elem_name == elem
    }

    pub fn camera_get_ccd_for_property(
        prop_name: &str,
        elem_name: &str,
    ) -> Option<CamCcd> {
        match (prop_name, elem_name) {
            ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE") =>
                Some(CamCcd::Main),
            ("GUIDER_EXPOSURE", "GUIDER_EXPOSURE_VALUE") =>
                Some(CamCcd::Guider),
            _ =>
                None,
        }
    }

    pub fn camera_get_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd
    ) -> Result<f64> {
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        self.get_num_property_value(
            device_name,
            prop_name,
            prop_elem
        )
    }

    pub fn camera_start_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd,
        exposure:    f64
    ) -> Result<()> {
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        self.command_set_num_property(
            device_name,
            prop_name,
            &[(prop_elem, exposure)]
        )?;
        Ok(())
    }

    pub fn camera_abort_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd,
    ) -> Result<()> {
        let prop_name = match ccd {
            CamCcd::Main   => "CCD_ABORT_EXPOSURE",
            CamCcd::Guider => "GUIDER_ABORT_EXPOSURE",
        };
        self.command_set_switch_property(
            device_name,
            prop_name,
            &[("ABORT", true)]
        )
    }

    fn exposure_prop_name(ccd: CamCcd) -> (&'static str, &'static str) {
        match ccd {
            CamCcd::Main   => ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE"),
            CamCcd::Guider => ("GUIDER_EXPOSURE", "GUIDER_EXPOSURE_VALUE"),
        }
    }

    // Cooler

    pub fn camera_is_cooler_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(device_name, "CCD_COOLER", None)
    }

    pub fn camera_is_cooler_on(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(device_name, "CCD_COOLER", "COOLER_ON")
    }

    pub fn camera_enable_cooler(
        &self,
        device_name: &str,
        enabled:     bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_COOLER", &[
            ("COOLER_ON",  enabled),
            ("COOLER_OFF", !enabled)
        ])
    }

    // CCD temperature

    pub fn camera_is_temperature_property(
        prop_name: &str,
        elem_name: &str,
    ) -> bool {
        PROP_CAM_TEMPERATURE.iter().any(|(prop, elem)|
            *prop == prop_name && *elem == elem_name
        )
    }

    pub fn camera_is_temperature_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_TEMPERATURE
        )
    }

    pub fn camera_get_temperature_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_TEMPERATURE
        )
    }

    pub fn camera_set_temperature(
        &self,
        device_name: &str,
        temperature: f64
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_TEMPERATURE,
            temperature,
            true,
            None
        )
    }

    // Camera cooling power

    pub fn camera_is_cooler_pwr_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_COOLING_PWR
        )
    }

    pub fn camera_is_cooler_pwr_property(
        prop_name: &str,
        elem_name: &str
    ) -> bool {
        PROP_CAM_COOLING_PWR.iter().any(|(prop, elem)|
            *prop == prop_name && *elem == elem_name
        )
    }

    pub fn camera_get_cooler_pwr_property(
        &self,
        device_name: &str
    ) -> Result<(Property, PropElement)> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, prop_elem) = devices.existing_prop_name(device_name, PROP_CAM_COOLING_PWR)?;
        self.get_property_and_element(&devices, device_name, prop_name, prop_elem)
    }

    fn get_property_and_element(
        &self,
        devices:     &Devices,
        device_name: &str,
        prop_name:   &str,
        prop_elem:   &str,
    ) -> Result<(Property, PropElement)> {
        let property = devices.get_property(device_name, prop_name)?;
        let elem = property.get_elem(prop_elem).ok_or_else(|| Error::PropertyElemNotExists(
            device_name.to_string(),
            prop_name.to_string(),
            prop_elem.to_string()
        ))?;
        Ok((property.clone(), elem.clone()))
    }


    // Camera fan

    pub fn camera_is_fan_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_FAN_ON
        )
    }

    pub fn camera_is_fan_str_property(
        prop_name: &str
    ) -> bool {
        PROP_CAM_FAN_ON.iter().any(|(prop, _)|
            *prop == prop_name
        )
    }

    pub fn camera_control_fan(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = if enable {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_OFF)?
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    // Camera window heater

    pub fn camera_is_heater_str_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_HEAT_CTRL_LIST
        )
    }

    pub fn camera_is_heater_str_property(
        prop_name: &str
    ) -> bool {
        PROP_CAM_HEAT_CTRL_LIST.iter().any(|(prop, _)|
            *prop == prop_name
        )
    }

    pub fn camera_get_heater_items(
        &self,
        device_name: &str
    ) -> Result<Vec<(Arc<String>, Arc<String>)>> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_HEAT_CTRL_LIST
        )?;
        let device = devices.find_by_name_res(device_name)?;
        let prop = device
            .get_property_opt(prop_name)
            .ok_or_else(|| Error::PropertyNotExists(device_name.to_string(), prop_name.to_string()))?;
        Ok(prop.elements
            .iter()
            .map(|e| {
                let name = Arc::clone(&e.name);
                let caption = Arc::clone(e.label.as_ref().unwrap_or(&e.name));
                (name, caption)
            })
            .collect()
        )
    }

    pub fn camera_set_heater_str(
        &self,
        device_name: &str,
        value:       &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_HEAT_CTRL_LIST
        )?;
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name, &[(value, true)]
        )?;
        Ok(())
    }


    // Camera low noise mode

    pub fn camera_is_low_noise_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_LOW_NOISE_ON
        )
    }

    pub fn camera_set_low_noise(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = if enable {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_OFF)?
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(())
    }


    // Conversion gain mode

    pub fn camera_is_conversion_gain_str_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_CONV_GAIN_LIST
        )
    }

    pub fn camera_is_conversion_gain_property(
        prop_name: &str
    ) -> bool {
        PROP_CAM_CONV_GAIN_LIST.iter().any(|(prop, _)|
            *prop == prop_name
        )
    }

    pub fn camera_get_conversion_gain_items(
        &self,
        device_name: &str
    ) -> Result<Vec<(Arc<String>, Arc<String>)>> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_CONV_GAIN_LIST
        )?;
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt(prop_name) else {
            return Err(Error::PropertyNotExists(device_name.to_string(), prop_name.to_string()));
        };
        Ok(prop.elements
            .iter()
            .map(|e| {
                let name = Arc::clone(&e.name);
                let caption = Arc::clone(e.label.as_ref().unwrap_or(&e.name));
                (name, caption)
            })
            .collect()
        )
    }

    pub fn camera_set_conversion_gain_str(
        &self,
        device_name: &str,
        value:       &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_CONV_GAIN_LIST
        )?;
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name, &[(value, true)]
        )?;
        Ok(())
    }


    // High fullwell mode

    pub fn camera_is_high_fullwell_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_HIGH_FULLWELL_ON
        )
    }

    pub fn camera_set_high_fullwell(
        &self,
        device_name: &str,
        on:          bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = if on {
            devices.existing_prop_name(device_name, PROP_CAM_HIGH_FULLWELL_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_HIGH_FULLWELL_OFF)?
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(())
    }


    // Camera gain

    pub fn camera_is_gain_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_GAIN
        )
    }

    pub fn camera_get_gain_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_GAIN
        )
    }

    pub fn camera_set_gain(
        &self,
        device_name: &str,
        gain:        f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_GAIN,
            gain,
            force_set,
            timeout_ms,
        )
    }

    // Camera offset

    pub fn camera_is_offset_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_OFFSET
        )
    }

    pub fn camera_get_offset_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_OFFSET
        )
    }

    pub fn camera_set_offset(
        &self,
        device_name: &str,
        offset:      f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_OFFSET,
            offset,
            force_set,
            timeout_ms
        )
    }

    // Camera capture format

    pub fn camera_is_capture_format_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_CAPTURE_FORMAT",
            Some("INDI_RAW")
        )
    }

    pub fn camera_set_video_format(
        &self,
        device_name: &str,
        format:      CaptureFormat,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = match format {
            CaptureFormat::Rgb =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RGB)?,
            CaptureFormat::Raw =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RAW)?,
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    pub fn camera_set_capture_format(
        &self,
        device_name: &str,
        format:      CaptureFormat,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let cap_elem = match format {
            CaptureFormat::Rgb => "INDI_RGB",
            CaptureFormat::Raw => "INDI_RAW",
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_CAPTURE_FORMAT",
            &[(cap_elem, true)]
        )?;
        Ok(())
    }

    // Camera resolution

    pub fn camera_is_resolution_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_RESOLUTION",
            None
        )
    }

    pub fn camera_get_supported_resolutions(
        &self,
        device_name: &str,
    ) -> Result<Vec<Arc<String>>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("CCD_RESOLUTION") else {
            return Ok(Vec::new());
        };
        Ok(prop.elements
            .iter()
            .map(|e| Arc::clone(&e.name))
            .collect())
    }

    pub fn camera_set_resolution(
        &self,
        device_name: &str,
        resolution:  &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_RESOLUTION",
            &[(resolution, true)]
        )
    }

    pub fn camera_get_resolution(
        &self,
        device_name: &str,
    ) -> Result<Option<Arc<String>>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("CCD_RESOLUTION") else {
            return Ok(None);
        };
        Ok(prop.elements
            .iter()
            .find(|e| e.value.to_i32().unwrap_or(0) != 0)
            .map(|e| Arc::clone(&e.name))
        )
    }

    pub fn camera_select_max_resolution(
        &self,
        device_name: &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let items = self.camera_get_supported_resolutions(device_name)?;
        if items.is_empty() { return Ok(false); }
        let values = items.iter().map(|s|{
            let mut splitted = s.split(['x', 'X']);
            let width: usize = splitted.next().map(|s| s.trim().parse().unwrap_or(0)).unwrap_or(0);
            let height: usize = splitted.next().map(|s| s.trim().parse().unwrap_or(0)).unwrap_or(0);
            (width + height, s)
        });
        let Some(max) = values.max_by_key(|item| item.0) else { return Ok(false); };
        self.camera_set_resolution(device_name, max.1, force_set, timeout_ms)?;
        Ok(true)
    }

    // Camera frame size and information

    pub fn camera_get_pixel_size_um(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(f64/*x*/, f64/*y*/)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_info_prop_name(cam_ccd);
        let size_x = devices.get_num_property(device_name, prop_name, "CCD_PIXEL_SIZE_X")?.value;
        let size_y = devices.get_num_property(device_name, prop_name, "CCD_PIXEL_SIZE_Y")?.value;
        Ok((size_x, size_y))
    }

    // CCD_FRAME

    pub fn camera_is_frame_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let res = devices.get_property(
            device_name,
            Self::ccd_frame_prop_name(cam_ccd)
        );
        match res {
            Err(e @ Error::DeviceNotExists(_)) => Err(e),
            Err(_) => Ok(false),
            Ok(s) => Ok(s.permission != PropPermission::RO),
        }
    }

    pub fn camera_set_frame(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
        x:           usize,
        y:           usize,
        width:       usize,
        height:      usize,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_frame_prop_name(cam_ccd), &[
            ("X",      x as f64),
            ("Y",      y as f64),
            ("WIDTH",  width as f64),
            ("HEIGHT", height as f64),
        ])
    }

    pub fn camera_get_max_frame_size(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_info_prop_name(cam_ccd);
        let width = devices.get_num_property(device_name, prop_name, "CCD_MAX_X")?.value;
        let height = devices.get_num_property(device_name, prop_name, "CCD_MAX_Y")?.value;
        Ok((width as usize, height as usize))
    }

    fn ccd_frame_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Main => "CCD_FRAME",
            CamCcd::Guider => "GUIDER_FRAME",
        }
    }

    fn ccd_info_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Main => "CCD_INFO",
            CamCcd::Guider => "GUIDER_INFO",
        }
    }

    // Camera binning

    pub fn camera_is_binning_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            Self::ccd_bin_prop_name(cam_ccd),
            None
        )
    }

    pub fn camera_get_max_binning(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_bin_prop_name(cam_ccd);
        if devices.property_exists(device_name, prop_name, None)? {
            let max_hor = devices.get_num_property(device_name, prop_name, "HOR_BIN")?.max;
            let max_vert = devices.get_num_property(device_name, prop_name, "VER_BIN")?.max;
            Ok((max_hor as usize, max_vert as usize))
        } else {
            Ok((1, 1))
        }
    }

    pub fn camera_set_binning(
        &self,
        device_name:    &str,
        cam_ccd:        CamCcd,
        horiz_binning: usize,
        vert_binning:  usize,
        force_set:      bool,
        timeout_ms:     Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_bin_prop_name(cam_ccd), &[
            ("HOR_BIN", horiz_binning as f64),
            ("VER_BIN", vert_binning as f64),
        ])
    }

    pub fn camera_get_binning(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_bin_prop_name(cam_ccd);
        if devices.property_exists(device_name, prop_name, None)? {
            let max_hor = devices.get_num_property(device_name, prop_name, "HOR_BIN")?.value;
            let max_vert = devices.get_num_property(device_name, prop_name, "VER_BIN")?.value;
            Ok((max_hor as usize, max_vert as usize))
        } else {
            Ok((1, 1))
        }
    }


    pub fn camera_is_binning_mode_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            Self::ccd_bin_mode_prop_name(cam_ccd),
            None
        )
    }

    pub fn camera_set_binning_mode(
        &self,
        device_name:  &str,
        binning_mode: BinningMode,
        force_set:    bool,
        timeout_ms:   Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = match binning_mode {
            BinningMode::Add =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_ADD)?,
            BinningMode::Avg =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_AVG)?,
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    pub fn camera_get_binning_mode(
        &self,
        device_name: &str,
    ) -> Result<Option<BinningMode>> {
        let is_add_mode = self.device_get_any_of_switch_props(
            device_name,
            PROP_CAM_BIN_ADD
        )?;
        let is_avg_mode = self.device_get_any_of_switch_props(
            device_name,
            PROP_CAM_BIN_AVG
        )?;
        if is_add_mode && !is_avg_mode {
            Ok(Some(BinningMode::Add))
        } else if !is_add_mode && is_avg_mode {
            Ok(Some(BinningMode::Avg))
        } else {
            Ok(None)
        }
    }

    fn ccd_bin_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Main => "CCD_BINNING",
            CamCcd::Guider => "GUIDER_BINNING",
        }
    }

    fn ccd_bin_mode_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Main => "CCD_BINNING_MODE",
            CamCcd::Guider => "GUIDER_BINNING_MODE",
        }
    }

    // Frame type (light, dark etc)

    pub fn camera_set_frame_type(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
        frame_type:  FrameType,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = match frame_type {
            FrameType::Light => "FRAME_LIGHT",
            FrameType::Flat => "FRAME_FLAT",
            FrameType::Dark => "FRAME_DARK",
            FrameType::Bias => "FRAME_BIAS",
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_frame_type_prop_name(cam_ccd),
            &[(elem_name, true)]
        )
    }

    fn ccd_frame_type_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Main => "CCD_FRAME_TYPE",
            CamCcd::Guider => "GUIDER_FRAME_TYPE",
        }
    }

    // Camera's telescope info

    pub fn camera_is_telescope_info_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(device_name, "SCOPE_INFO", None)
    }

    pub fn camera_get_telescope_focal_len_and_aperture(&self, device_name: &str) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let focal_len = devices.get_num_property(device_name, "SCOPE_INFO", "FOCAL_LENGTH")?.value;
        let aperture = devices.get_num_property(device_name, "SCOPE_INFO", "APERTURE")?.value;
        Ok((focal_len, aperture))
    }

    pub fn camera_set_telescope_focal_len_and_aperture(
        &self,
        device_name:  &str,
        focal_length: f64,
        aperture:     Option<f64>,
        force_set:    bool,
        timeout_ms:   Option<u64>,
    ) -> Result<()> {
        let mut cur_aperture = self.get_num_property_value(
            device_name,
            "SCOPE_INFO",
            "APERTURE"
        )?;
        if let Some(aperture) = aperture {
            cur_aperture = aperture;
        }
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "SCOPE_INFO",
            &[("FOCAL_LENGTH", focal_length),
              ("APERTURE",     cur_aperture)]
        )?;
        Ok(())
    }

    // Focuser absolute position

    pub fn focuser_get_abs_value_prop_elem(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            &[("ABS_FOCUS_POSITION", "FOCUS_ABSOLUTE_POSITION")]
        )
    }

    pub fn focuser_get_abs_value_prop(
        &self,
        device_name: &str
    ) -> Result<Property> {
        let devices = self.devices.lock().unwrap();
        let prop = devices.get_property(device_name, "ABS_FOCUS_POSITION")?;
        Ok(prop.clone())
    }


    pub fn focuser_get_max(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "FOCUS_MAX",
            "FOCUS_MAX_VALUE"
        )
    }

    pub fn focuser_get_temperature(&self, device_name: &str) -> Result<f64> {
        Ok(self.device_get_num_prop(
            device_name,
            &[
                ("FOCUS_TEMPERATURE", "TEMPERATURE"),
                ("FOCUS_TEMPERATURE", "FOCUS_TEMPERATURE"),
            ]
        )?.value)
    }

    pub fn focuser_set_abs_value(
        &self,
        device_name: &str,
        value:       f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "ABS_FOCUS_POSITION",
            &[("FOCUS_ABSOLUTE_POSITION", value)]
        )
    }

    pub fn mount_abort_motion(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_ABORT_MOTION",
            &[("ABORT", true)]
        )
    }

    pub fn mount_get_eq_coord_prop_state(&self, device_name: &str) -> Result<PropState> {
        let devices = self.devices.lock().unwrap();
        let state = devices.get_property(device_name, "EQUATORIAL_EOD_COORD")?.state;
        Ok(state)
    }

    pub fn mount_get_eq_dec(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "EQUATORIAL_EOD_COORD",
            "DEC"
        )
    }

    pub fn mount_get_eq_ra(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "EQUATORIAL_EOD_COORD",
            "RA"
        )
    }

    pub fn mount_get_eq_ra_and_dec(&self, device_name: &str) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ra = devices.get_num_property(device_name, "EQUATORIAL_EOD_COORD", "RA")?.value;
        let dec = devices.get_num_property(device_name, "EQUATORIAL_EOD_COORD", "DEC")?.value;
        Ok((ra, dec))
    }

    pub fn mount_set_after_coord_action(
        &self,
        device_name:  &str,
        after_action: AfterCoordSetAction,
        force_set:    bool,
        timeout_ms:   Option<u64>,
    ) -> Result<()> {
        let after_action_elem = match after_action {
            AfterCoordSetAction::Track => "TRACK",
            AfterCoordSetAction::Slew => "SLEW",
            AfterCoordSetAction::Sync => "SYNC",
        };

        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "ON_COORD_SET",
            &[(after_action_elem, true)]
        )
    }

    pub fn mount_set_eq_coord(
        &self,
        device_name: &str,
        ra:          f64,
        dec:         f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "EQUATORIAL_EOD_COORD", &[
            ("RA",  ra),
            ("DEC", dec),
        ])
    }

    pub fn mount_start_move_north(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_NS",
            &[("MOTION_NORTH", true), ("MOTION_SOUTH", false)]
        )
    }

    pub fn mount_start_move_south(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_NS",
            &[("MOTION_NORTH", false), ("MOTION_SOUTH", true)]
        )
    }

    pub fn mount_start_move_west(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_WE",
            &[("MOTION_WEST", true), ("MOTION_EAST", false)]
        )
    }

    pub fn mount_start_move_east(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_WE",
            &[("MOTION_WEST", false), ("MOTION_EAST", true)]
        )
    }

    pub fn mount_is_moving(&self, device_name: &str) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let moving_north = devices.get_switch_property(device_name, "TELESCOPE_MOTION_NS", "MOTION_NORTH")?;
        let moving_south = devices.get_switch_property(device_name, "TELESCOPE_MOTION_NS", "MOTION_SOUTH")?;
        let moving_west = devices.get_switch_property(device_name, "TELESCOPE_MOTION_WE", "MOTION_WEST")?;
        let moving_east = devices.get_switch_property(device_name, "TELESCOPE_MOTION_WE", "MOTION_EAST")?;
        Ok(moving_north || moving_south || moving_west || moving_east)
    }

    pub fn mount_revert_motion(
        &self,
        device_name: &str,
        reverse_ns:  bool,
        reverse_we:  bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_REVERSE_MOTION", &[
            ("REVERSE_NS", reverse_ns),
            ("REVERSE_WE", reverse_we),
        ])
    }

    pub fn mount_get_slew_speed_list(
        &self,
        device_name: &str
    ) -> Result<Vec<(Arc<String>, Option<Arc<String>>)>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("TELESCOPE_SLEW_RATE") else {
            return Ok(Vec::new());
        };
        let result = prop.elements
            .iter()
            .map(|e| (Arc::clone(&e.name), e.label.clone()))
            .collect();
        Ok(result)
    }

    pub fn mount_set_slew_speed(
        &self,
        device_name: &str,
        speed_name:  &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_SLEW_RATE",
            &[(speed_name, true)]
        )
    }

    pub fn mount_is_tracking(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "TELESCOPE_TRACK_STATE",
            "TRACK_ON"
        )
    }

    pub fn mount_set_tracking(
        &self,
        device_name: &str,
        tracking:    bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = if tracking {
            "TRACK_ON"
        } else {
            "TRACK_OFF"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_TRACK_STATE",
            &[(elem_name, true)]
        )
    }

    pub fn mount_is_parked(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "TELESCOPE_PARK",
            "PARK"
        )
    }

    pub fn mount_set_parked(
        &self,
        device_name: &str,
        parked:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = if parked {
            "PARK"
        } else {
            "UNPARK"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_PARK",
            &[(elem_name, true)]
        )
    }

    pub fn mount_get_timed_guide_max(
        &self,
        device_name: &str
    ) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ns_items = &devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_NS")?.elements;
        let we_items = &devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_WE")?.elements;
        if ns_items.is_empty() || we_items.is_empty() {
            return Err(Error::Internal("Wrong prop elem len".into()));
        }
        let PropValue::Num(NumPropValue{max: ns_max, ..}) = &ns_items[0].value else {
            return Err(Error::Internal("Wrong prop elem type".into()));
        };
        let PropValue::Num(NumPropValue{max: we_max, ..}) = &we_items[0].value else {
            return Err(Error::Internal("Wrong prop elem type".into()));
        };
        Ok((*ns_max, *we_max))
    }

    pub fn mount_timed_guide(
        &self,
        device_name: &str,
        north_south: f64,
        west_east:   f64,
    ) -> Result<()> {
        let (north, south) = if north_south > 0.0 {
            (north_south, 0.0)
        } else {
            (0.0, -north_south)
        };
        let (west, east) = if west_east > 0.0 {
            (west_east, 0.0)
        } else {
            (0.0, -west_east)
        };
        self.command_set_num_property(
            device_name,
            "TELESCOPE_TIMED_GUIDE_NS", &[
            ("TIMED_GUIDE_N", north),
            ("TIMED_GUIDE_S", south),
        ])?;
        self.command_set_num_property(
            device_name,
            "TELESCOPE_TIMED_GUIDE_WE",&[
            ("TIMED_GUIDE_W", west),
            ("TIMED_GUIDE_E", east),
        ])?;
        Ok(())
    }

    pub fn mount_is_timed_guiding(&self, device_name: &str) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let property_ns = devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_NS")?;
        let property_we = devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_WE")?;
        let result =
            matches!(property_ns.state, PropState::Busy) &&
            matches!(property_we.state, PropState::Busy);
        Ok(result)
    }

    pub fn mount_is_guide_rate_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "GUIDE_RATE",
            None
        )
    }

    pub fn mount_get_guide_rate_prop_data(
        &self,
        device_name: &str
    ) -> Result<Property> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_property(device_name, "GUIDE_RATE")?;
        Ok(property.clone())
    }

    pub fn mount_get_guide_rate_ns(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "GUIDE_RATE",
            "GUIDE_RATE_NS"
        )
    }

    pub fn mount_get_guide_rate_we(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "GUIDE_RATE",
            "GUIDE_RATE_WE"
        )
    }

    pub fn mount_get_guide_rate(
        &self,
        device_name: &str,
    ) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ns = devices.get_num_property(device_name, "GUIDE_RATE", "GUIDE_RATE_NS")?.value;
        let we = devices.get_num_property(device_name, "GUIDE_RATE", "GUIDE_RATE_WE")?.value;
        Ok((ns, we))
    }

    pub fn mount_set_guide_rate(
        &self,
        device_name: &str,
        rate_ns:     f64,
        rate_we:     f64,
        force_set:   bool,
        timeout_ms:  Option<u64>
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "GUIDE_RATE", &[
            ("GUIDE_RATE_NS", rate_ns),
            ("GUIDE_RATE_WE", rate_we),
        ])?;
        Ok(())
    }

    pub fn get_geo_lat_long_elev(&self, device_name: &str) -> Result<(f64, f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let latitude = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "LAT")?.value;
        let longitude = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "LONG")?.value;
        let elevation = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "ELEV")?.value;
        Ok((latitude, longitude, elevation))
    }

    pub fn filter_get_list_and_active(&self, device_name: &str) -> Result<(Vec<Arc<String>>, usize)> {
        let mut result = Vec::new();
        let devices = self.devices.lock().unwrap();
        let active_prop = devices.get_num_property(device_name, "FILTER_SLOT", "FILTER_SLOT_VALUE")?;
        let min_filter = active_prop.min as i32;
        let max_filter = active_prop.max as i32;
        for i in min_filter..=max_filter {
            let elem_name = format!("FILTER_SLOT_NAME_{i}");
            result.push(devices.get_text_property(device_name, "FILTER_NAME", &elem_name)?);
        }
        let active_num = active_prop.value as i32;
        let active_id = if active_num >= min_filter { active_num - min_filter } else { 0 };
        Ok((result, active_id as _))
    }

    pub fn filter_get_active(&self, device_name: &str) -> Result<i32> {
        let devices = self.devices.lock().unwrap();
        let result = devices.get_num_property(device_name, "FILTER_SLOT", "FILTER_SLOT_VALUE")?.value as i32;
        Ok(result)
    }

    pub fn filter_set_active(&self, device_name: &str, active_id: i32) -> Result<()> {
        let mut devices = self.devices.lock().unwrap();
        let cur_prop = devices.get_num_property(device_name, "FILTER_SLOT", "FILTER_SLOT_VALUE")?;
        let new_value = cur_prop.min as i32 + active_id;
        self.command_set_num_property_impl(
            &mut devices,
            device_name,
            "FILTER_SLOT",
            &[("FILTER_SLOT_VALUE", new_value as _)]
        )?;
        Ok(())
    }
}

struct XmlSender {
    xml_sender: mpsc::Sender<XmlSenderItem>,
}

impl XmlSender {
    fn main(receiver: mpsc::Receiver<XmlSenderItem>, stream: Stream) {
        fn send_xml<T: Write>(
            writer: &mut T,
            xml:    String
        ) -> std::result::Result<(), std::io::Error> {
            writer.write_all(xml.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            Ok(())
        }
        let mut writer = BufWriter::new(stream.as_write_box());
        while let Ok(item) = receiver.recv() {
            match item {
                XmlSenderItem::Xml(xml) => {
                    let res = send_xml(&mut writer, xml);
                    if res.is_err() { break; }
                },
                XmlSenderItem::Exit => {
                    break;
                }
            }
        }
    }

    fn send_exit_to_thread(&self) {
        _ = self.xml_sender.send(XmlSenderItem::Exit);
    }

    fn send_xml(
        &self,
        xml: &xmltree::Element
    ) -> Result<()> {
        let mut mem_buf = Cursor::new(Vec::new());
        let mut xml_conf = xmltree::EmitterConfig::new();
        xml_conf.write_document_declaration = false;
        xml.write_with_config(&mut mem_buf, xml_conf)
            .map_err(|e| Error::Internal(e.to_string()))?;
        let xml_text = String::from_utf8(mem_buf.into_inner())
            .map_err(|e| Error::Internal(e.to_string()))?;
        if log::log_enabled!(log::Level::Trace) {
            log::trace!("indi_api: outgoing xml =\n{}", xml_text);
        }
        self.xml_sender.send(XmlSenderItem::Xml(xml_text))
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    fn command_set_property_impl<'a>(
        &self,
        device_name:    &str,
        prop_name:      &str,
        command_tag:    &str,
        elem_tag:       &str,
        elem_count:     usize,
        elem_get_name:  impl Fn(usize) -> &'a str,
        elem_get_value: impl Fn(usize) -> String,
    ) -> Result<()> {
        // Send XML with new property data
        let mut xml_command = xmltree::Element::new(command_tag);
        xml_command.attributes.insert("device".to_string(), device_name.to_string());
        xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        for index in 0..elem_count {
            let mut xml_elem = xmltree::Element::new(elem_tag);
            xml_elem.attributes.insert("name".to_string(), elem_get_name(index).to_string());
            xml_elem.children.push(xmltree::XMLNode::Text(elem_get_value(index)));
            xml_command.children.push(xmltree::XMLNode::Element(xml_elem));
        }
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_get_properties_impl(
        &self,
        device_name: Option<&str>,
        prop_name:   Option<&str>
    ) -> Result<()> {
        let mut xml_command = xmltree::Element::new("getProperties");
        xml_command.attributes.insert("version".to_string(), "1.7".to_string());
        if let Some(device_name) = device_name {
            xml_command.attributes.insert("device".to_string(), device_name.to_string());
        }
        if let Some(prop_name) = prop_name {
            xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        }
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_enable_blob(
        &self,
        device_name: &str,
        prop_name:   Option<&str>,
        mode:        BlobEnable
    ) -> Result<()> {
        let mut xml_command = xmltree::Element::new("enableBLOB");
        xml_command.attributes.insert("device".to_string(), device_name.to_string());
        if let Some(prop_name) = prop_name {
            xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        }
        let mode_str = match mode {
            BlobEnable::Never => "Never",
            BlobEnable::Also => "Also",
            BlobEnable::Only => "Only",
        };
        xml_command.children.push(xmltree::XMLNode::Text(mode_str.to_string()));
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_ping_reply(&self, id: &str) -> Result<()> {
        let xml_text = format!("<pingReply uid=\"{id}\"/>");
        self.xml_sender.send(XmlSenderItem::Xml(xml_text))
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    fn command_set_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, &str)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newTextVector",
            "oneText",
            elements.len(),
            |index| elements[index].0,
            |index| elements[index].1.to_string(),
        )
    }

    fn command_set_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newSwitchVector",
            "oneSwitch",
            elements.len(),
            |index| elements[index].0,
            |index| if elements[index].1 { "On".to_string() } else { "Off".to_string() },
        )
    }

    fn command_set_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newNumberVector",
            "oneNumber",
            elements.len(),
            |index| elements[index].0,
            |index| elements[index].1.to_string(),
        )
    }

    fn command_enable_device(
        &self,
        device_name: &str,
        enable:      bool,
    ) -> Result<()> {
        let elem = if enable {
            "CONNECT"
        } else {
            "DISCONNECT"
        };
        self.command_set_switch_property(
            device_name,
            "CONNECTION",
            &[(elem, true)]
        )?;
        Ok(())
    }
}

struct XmlReceiver {
    conn_state:      Arc<Mutex<ConnState>>,
    devices:         Arc<Mutex<Devices>>,
    stream:          Stream,
    reader:          XmlStreamReader,
    xml_sender:      XmlSender,
}

impl XmlReceiver {
    fn new(
        conn_state: &Arc<Mutex<ConnState>>,
        devices:    &Arc<Mutex<Devices>>,
        stream:     Stream,
        xml_sender: XmlSender,
    ) -> Self {
        Self {
            conn_state: Arc::clone(conn_state),
            devices:    Arc::clone(devices),
            reader:     XmlStreamReader::new(),
            stream,
            xml_sender,
        }
    }

    fn main(&mut self, events_sender: mpsc::Sender<EventSenderEvent>) {
        self.stream.set_read_timeout(Some(Duration::from_millis(1000))).unwrap(); // TODO: check error

        self.xml_sender.command_get_properties_impl(None, None).unwrap(); // TODO: check error

        loop {
            let stream_for_xml = match &self.stream {
                Stream::Tcp(_) => XmlStream::Read(self.stream.as_read()),
                #[cfg(target_os = "linux")]
                Stream::Unix(unix_stream) => XmlStream::Unix(unix_stream),
            };
            let xml_res = self.reader.receive_xml(stream_for_xml);
            match xml_res {
                Ok(XmlStreamReaderResult::BlobBegin {
                    device_name, prop_name, elem_name, format, len
                }) => {
                    let device_name = Arc::new(device_name);
                    let prop_name = Arc::new(prop_name);
                    let elem_name = Arc::new(elem_name);
                    self.notify_subscribers_about_blob_start(
                        &device_name,
                        &prop_name,
                        &elem_name,
                        &format,
                        len,
                        &events_sender,
                    );
                }
                Ok(XmlStreamReaderResult::Xml{ xml, blobs }) => {
                    if log::log_enabled!(log::Level::Trace) {
                        log::trace!("indi_api: incoming xml =\n{}", xml);
                    }
                    let process_xml_res = self.process_xml(&xml, blobs, &events_sender);
                    if let Err(err) = process_xml_res {
                        log::debug!("indi_api: '{}' for XML\n{}", err, xml);
                    } else {
                        let mut state = self.conn_state.lock().unwrap();
                        if *state == ConnState::Connecting {
                            *state = ConnState::Connected;
                            drop(state);
                            events_sender.send(EventSenderEvent::Mess(Event::ConnChange(
                                ConnState::Connected
                            ))).unwrap();
                        }
                    }
                }
                Ok(XmlStreamReaderResult::Disconnected) => {
                    log::debug!("indi_api: Disconnected");
                    _ = events_sender.send(EventSenderEvent::Mess(Event::ConnChange(
                        ConnState::Disconnected
                    )));
                    break;
                }
                Ok(XmlStreamReaderResult::TimeOut) => {
                    /* do nothing */
                }
                Err(err) => {
                    self.reader.recover_after_error();
                    log::error!("indi_api: {}", err);
                },
            }
        }
    }

    fn update_device_info(
        &self,
        device:         &mut Device,
        prop_name:      &str,
        changed_values: &Vec<(Arc<String>, PropValue)>
    ) {
        for (name, value) in changed_values {
            if prop_name == "CONNECTION"
            && name.as_str() == "CONNECT" {
                device.set_connected(value.to_bool().unwrap_or(false));
            }

            if prop_name == "DRIVER_INFO"
            && name.as_str() == "DRIVER_INTERFACE" {
                let flag_bits = value.to_i32().unwrap_or(0);
                device.set_driver_interface(DriverInterface::from_bits_truncate(flag_bits as u32));
            }
        }
    }

    fn notify_subscribers_about_new_prop(
        &self,
        device_name:    &Arc<String>,
        is_connected:   bool,
        timestamp:      Option<DateTime<Utc>>,
        prop_name:      &Arc<String>,
        state:          PropState,
        changed_values: Vec<(Arc<String>, PropValue)>,
        events_sender:  &mpsc::Sender<EventSenderEvent>,
    ) {
        for (name, value) in changed_values {
            let change = PropChange::New {
                prop_name:  Arc::clone(prop_name),
                elem_name:  Arc::clone(&name),
                value:      value.clone(),
                state
            };
            events_sender.send(EventSenderEvent::Mess(Event::PropChange(PropChangeEvent {
                timestamp,
                device_name: Arc::clone(device_name),
                change,
            }))).unwrap();

            if prop_name.as_str() == "DRIVER_INFO"
            && name.as_str() == "DRIVER_INTERFACE" {
                let flag_bits = value.to_i32().unwrap_or(0);
                let interface = DriverInterface::from_bits_truncate(flag_bits as u32);
                let event_data = NewDeviceEvent {
                    device_name: Arc::clone(device_name),
                    connected: is_connected,
                    interface,
                    timestamp,
                };
                events_sender.send(EventSenderEvent::Mess(Event::NewDevice(
                    event_data
                ))).unwrap();
            }
        }
    }

    fn notify_subscribers_about_prop_change(
        &self,
        timestamp:      Option<DateTime<Utc>>,
        device:         &mut Device,
        prop_name:      &Arc<String>,
        prev_state:     PropState,
        new_state:      PropState,
        changed_values: Vec<(Arc<String>, PropValue)>,
        events_sender:  &mpsc::Sender<EventSenderEvent>
    ) {
        for (name, prop_value) in changed_values {
            let change = PropChange::Change{
                prop_name:  Arc::clone(prop_name),
                elem_name:  Arc::clone(&name),
                value:      prop_value.clone(),
                prev_state,
                new_state,
            };
            events_sender.send(EventSenderEvent::Mess(Event::PropChange(PropChangeEvent {
                timestamp,
                device_name: Arc::clone(device.name()),
                change,
            }))).unwrap();

            if prop_name.as_str() == "CONNECTION"
            && name.as_str() == "CONNECT" {
                let connected = prop_value.to_bool().unwrap_or(false);

                let event_data = DeviceConnectEvent {
                    device_name: Arc::clone(device.name()),
                    interface: device.driver_interface(),
                    timestamp,
                    connected,
                };

                events_sender.send(EventSenderEvent::Mess(Event::DeviceConnected(
                    event_data
                ))).unwrap();
            }
        }
    }

    fn notify_subscribers_about_prop_delete(
        &self,
        time:          Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        prop_name:     &Arc<String>,
        events_sender: &mpsc::Sender<EventSenderEvent>
    ) {
        let change = PropChange::Delete {
            prop_name: Arc::clone(prop_name)
        };
        events_sender.send(EventSenderEvent::Mess(Event::PropChange(PropChangeEvent {
            timestamp:   time,
            device_name: Arc::clone(device_name),
            change,
        }))).unwrap();
    }

    fn notify_subscribers_about_device_delete(
        &self,
        time:          Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        events_sender: &mpsc::Sender<EventSenderEvent>,
        drv_interface: DriverInterface,
    ) {
        events_sender.send(EventSenderEvent::Mess(Event::DeviceDelete(DeviceDeleteEvent {
            timestamp:   time,
            device_name: Arc::clone(device_name),
            interface:   drv_interface
        }))).unwrap();
    }

    fn notify_subscribers_about_message(
        &self,
        timestamp:     Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        message:       &Arc<String>,
        events_sender: &mpsc::Sender<EventSenderEvent>
    ) {
        events_sender.send(EventSenderEvent::Mess(Event::Message(MessageEvent {
            timestamp,
            device_name: Arc::clone(device_name),
            text:        Arc::clone(message),
        }))).unwrap();
    }

    fn notify_subscribers_about_blob_start(
        &self,
        device_name:   &Arc<String>,
        prop_name:     &Arc<String>,
        elem_name:     &Arc<String>,
        format:        &str,
        len:           Option<usize>,
        events_sender: &mpsc::Sender<EventSenderEvent>
    ) {
        events_sender.send(EventSenderEvent::Mess(Event::BlobStart(BlobStartEvent {
            device_name: Arc::clone(device_name),
            prop_name:   Arc::clone(prop_name),
            elem_name:   Arc::clone(elem_name),
            format:      Arc::new(format.to_string()),
            len,
        }))).unwrap();
    }

    fn process_xml(
        &mut self,
        xml_text:      &str,
        blobs:         Vec<XmlStreamReaderBlob>,
        events_sender: &mpsc::Sender<EventSenderEvent>,
    ) -> eyre::Result<()> {
        let mut xml_elem = xmltree::Element::parse(xml_text.as_bytes())?;
        if xml_elem.name == "pingRequest" {
            let uid = xml_elem.attr_str("uid").unwrap_or_default();
            self.xml_sender.command_ping_reply(uid)?;
        }
        else if xml_elem.name.starts_with("def") { // defXXXXVector
            // New property from INDI server
            let device_name = xml_elem.attr_string_or_err("device")?;
            if device_name.is_empty() {
                eyre::bail!("Empty device name");
            }
            let mut devices = self.devices.lock().unwrap();

            let change_id = devices.next_change_id();

            let device_name = Arc::new(device_name);
            let device = if let Some(device) = devices.find_by_name_opt_mut(&device_name) {
                device
            } else {
                devices.add(Device::new(&device_name))
            };
            let prop_name = xml_elem.attr_string_or_err("name")?;
            if device.get_property_opt(&prop_name).is_some() {
                // simply ignore if INDI server sends defXXXXVector command
                // for already existing property
                return Ok(());
            }
            let timestamp = xml_elem.attr_time("timestamp");
            let mut property = Property::new_from_xml(
                xml_elem,
                device.name(),
                &prop_name
            )?;
            let state = property.state;
            let values = property.get_values();
            property.change_id = change_id;
            let prop_name = Arc::clone(&property.name);
            device.add_property(property);
            self.update_device_info(device, &prop_name, &values);
            let device_name = Arc::clone(device.name());
            let is_connected = device.is_connected();
            drop(devices);
            self.notify_subscribers_about_new_prop(
                &device_name,
                is_connected,
                timestamp,
                &prop_name,
                state,
                values,
                events_sender,
            );
        } else if xml_elem.name.starts_with("set") { // setXXXXVector
            // Changed property data from INDI server
            let device_name = xml_elem.attr_string_or_err("device")?;
            let prop_name = xml_elem.attr_string_or_err("name")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let mut devices = self.devices.lock().unwrap();
            let change_id = devices.next_change_id();
            let Some(device) = devices.find_by_name_opt_mut(&device_name) else {
                eyre::bail!(Error::DeviceNotExists(device_name));
            };
            let device_name = Arc::clone(device.name());
            let Some(property) = device.get_property_opt_mut(&prop_name) else {
                eyre::bail!(Error::PropertyNotExists(
                    device_name.to_string(),
                    prop_name
                ));
            };
            property.change_id = change_id;
            let prev_state = property.state;
            let (prop_changed, mut values) = property.update_data_from_xml_and_return_changes(
                &mut xml_elem,
                blobs,
                &device_name,
                &prop_name,
            )?;
            if prop_changed {
                let prop_name = Arc::clone(&property.name);
                let cur_state = property.state;
                if values.is_empty() && prev_state != cur_state {
                    values = property.get_values();
                }
                self.update_device_info(device, &prop_name, &values);
                self.notify_subscribers_about_prop_change(
                    timestamp,
                    device,
                    &prop_name,
                    prev_state,
                    cur_state,
                    values,
                    events_sender,
                );
                drop(devices);
            }
        } else if xml_elem.name == "delProperty" { // delProperty
            let device_name = xml_elem.attr_string_or_err("device")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let mut devices = self.devices.lock().unwrap();
            if let Some(prop_name) = xml_elem.attributes.remove("name") {
                let Some(device) = devices.find_by_name_opt_mut(&device_name) else {
                    eyre::bail!(Error::DeviceNotExists(device_name));
                };
                let dev_name_arc = Arc::clone(device.name());
                let removed_prop = device.remove_property(&prop_name)
                    .ok_or_else(
                        || Error::PropertyNotExists(device_name.clone(), prop_name.clone())
                    )?;
                self.notify_subscribers_about_prop_delete(
                    timestamp,
                    &dev_name_arc,
                    &removed_prop.name,
                    events_sender
                );
            } else {
                let drv_info = devices.get_driver_info(&device_name)?;
                let Some(removed) = devices.remove(&device_name) else {
                    eyre::bail!(Error::DeviceNotExists(device_name));
                };
                self.notify_subscribers_about_device_delete(
                    timestamp,
                    removed.name(),
                    events_sender,
                    drv_info.interface
                );
            }
        // message
        } else if xml_elem.name == "message" {
            let message = xml_elem.attr_string_or_err("message")?;
            let device = xml_elem.attr_string_or_err("device")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let device = Arc::new(device);
            let message = Arc::new(message);
            self.notify_subscribers_about_message(timestamp, &device, &message, events_sender);
        } else if !matches!(xml_elem.name.as_str(), "newTextVector"|"newNumberVector"|"newSwitchVector"|"newBLOBVector") {
            log::error!("Unknown tag: {}, xml=\n{}", xml_elem.name, xml_text);
        }
        Ok(())
    }
}

pub type PropsNamePair = (&'static str, &'static str);
pub type PropsNamePairs = &'static [PropsNamePair];

const PROP_CAM_TEMPERATURE: PropsNamePairs = &[
    ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE"),
];
const PROP_CAM_COOLING_PWR: PropsNamePairs = &[
    ("COOLER_POWER",     "COOLER_POWER"),
    ("CCD_COOLER_POWER", "COOLER_POWER"),
    ("CCD_COOLER_POWER", "CCD_COOLER_VALUE")
];
const PROP_CAM_GAIN: PropsNamePairs = &[
    ("CCD_GAIN",     "GAIN"),
    ("CCD_CONTROLS", "Gain"),
];
const PROP_CAM_OFFSET: PropsNamePairs = &[
    ("CCD_OFFSET",   "OFFSET"),
    ("CCD_CONTROLS", "Offset"),
];
const PROP_CAM_FAN_ON: PropsNamePairs = &[
    ("TC_FAN_CONTROL", "TC_FAN_ON"),
    ("TC_FAN_SPEED",   "INDI_ENABLED"),
];
const PROP_CAM_FAN_OFF: PropsNamePairs = &[
    ("TC_FAN_CONTROL", "TC_FAN_OFF"),
    ("TC_FAN_SPEED",   "INDI_DISABLED"),
];
const PROP_CAM_HEAT_CTRL_LIST: PropsNamePairs = &[
    ("TC_HEAT_CONTROL", ""),
];
const PROP_CAM_LOW_NOISE_ON: PropsNamePairs = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_ENABLED"),
    ("TC_LOW_NOISE",         "INDI_ENABLED"),
];
const PROP_CAM_LOW_NOISE_OFF: PropsNamePairs = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_DISABLED"),
    ("TC_LOW_NOISE",         "INDI_DISABLED"),
];
const PROP_CAM_VIDEO_FORMAT_RGB: PropsNamePairs = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RGB"),
];
const PROP_CAM_VIDEO_FORMAT_RAW: PropsNamePairs = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RAW"),
];
const PROP_CAM_BIN_AVG: PropsNamePairs = &[
    ("CCD_BINNING_MODE", "TC_BINNING_AVG"),
];
const PROP_CAM_BIN_ADD: PropsNamePairs = &[
    ("CCD_BINNING_MODE", "TC_BINNING_ADD"),
];
const PROP_DEVICE_CRASH: PropsNamePairs = &[
    ("CCD_SIMULATE_CRASH", "CRASH"),
];

const PROP_CAM_CONV_GAIN_LIST: PropsNamePairs = &[
    ("TC_CONVERSION_GAIN", ""),
];

const PROP_CAM_HIGH_FULLWELL_ON: PropsNamePairs = &[
    ("TC_HIGHFULLWELL", "INDI_ENABLED"),
];

const PROP_CAM_HIGH_FULLWELL_OFF: PropsNamePairs = &[
    ("TC_HIGHFULLWELL", "INDI_DISABLED"),
];

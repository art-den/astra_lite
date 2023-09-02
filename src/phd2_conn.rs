use std::{
    net::{TcpStream, Shutdown},
    io::*,
    sync::atomic::{AtomicBool, Ordering}, sync::{Arc, Mutex, RwLock},
    thread::{JoinHandle, spawn},
    time::Duration
};


#[derive(Debug, Clone)]
pub enum Phd2Event {
    Started,
    Stopped,
    Connected,
    Disconnected,
    Version,
}

type Phd2EventFun = dyn Fn(Phd2Event) + 'static + Send + Sync;

pub struct Phd2Conn {
    exit_flag:      Arc<AtomicBool>,
    main_thread:    Arc<Mutex<Option<JoinHandle<()>>>>,
    send_stream:    Arc<Mutex<Option<TcpStream>>>,
    event_handlers: Arc<RwLock<Vec<Box<Phd2EventFun>>>>,
}

impl Phd2Conn {
    pub fn new() -> Self {
        Self {
            exit_flag:      Arc::new(AtomicBool::new(false)),
            main_thread:    Arc::new(Mutex::new(None)),
            send_stream:    Arc::new(Mutex::new(None)),
            event_handlers: Arc::new(RwLock::new(Vec::new())),
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
        let self_send_stream = Arc::clone(&self.send_stream);
        let event_handlers = Arc::clone(&self.event_handlers);
        let host = host.to_string();

        let main_thread = spawn(move || {
            log::debug!("Begin PHD2 stream");
            Self::notify_event(&event_handlers, Phd2Event::Started);

            // Main loop
            loop {
                // Connecting...
                let read_stream = 'connect_loop: loop {
                    let conn_result = TcpStream::connect(&host_and_port_string);
                    match conn_result {
                        Ok(conn_result) =>
                            break Some(conn_result),
                        Err(_) => {
                            for _ in 0..10 { // wait 1000 ms before next try to connect
                                if exit_flag.load(Ordering::Relaxed) {
                                    break 'connect_loop None;
                                }
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            continue;
                        }
                    }
                };
                let Some(mut read_stream) = read_stream else {
                     break;
                };
                let Ok(send_stream) = read_stream.try_clone() else {
                    break;
                };

                Self::notify_event(&event_handlers, Phd2Event::Connected);
                log::debug!("Connected to PHD2 at {}:{}", host, port);

                *self_send_stream.lock().unwrap() = Some(send_stream);

                // Reading PHD2's jsons
                let mut buffer = Vec::new();
                let mut read_buffer = [0_u8; 1024];
                loop {
                    let read = match read_stream.read(&mut read_buffer) {
                        Ok(read) => read,
                        Err(err) => {
                            log::debug!("PHD2 read_stream.read returned {}", err.to_string());
                            break;
                        }
                    };
                    if read == 0 { break; }
                    buffer.extend_from_slice(&read_buffer[..read]);
                    if let Some(endl_pos) = buffer.iter().position(|v| *v == b'\n') {
                        let Ok(js_str) = std::str::from_utf8(&buffer[..=endl_pos]) else {
                            continue;
                        };
                        println!("{}", js_str);
                        if let Ok(_js) = json::parse(js_str) {

                        }
                        buffer.drain(..=endl_pos);
                    }
                }

                Self::notify_event(&event_handlers, Phd2Event::Disconnected);

                let exit_flag = exit_flag.load(Ordering::Relaxed);
                log::debug!("Exited from reading PHD2 stream, exit_flag = {}", exit_flag);

                *self_send_stream.lock().unwrap() = None;

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
        let mut self_send_stream = self.send_stream.lock().unwrap();
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

    pub fn command_pause(&self, _pause: bool) -> anyhow::Result<()> {
        log::debug!("Phd2Conn::command_pause, pause = {}", _pause);

        let mut self_send_stream = self.send_stream.lock().unwrap();
        if let Some(_send_stream) = &mut *self_send_stream {
            Ok(())
        } else {
            anyhow::bail!("Not working");
        }
    }

    fn notify_event(event_handlers: &Arc<RwLock<Vec<Box<Phd2EventFun>>>>, event: Phd2Event) {
        let event_handlers = event_handlers.read().unwrap();
        for handler in &*event_handlers {
            handler(event.clone());
        }
    }
}
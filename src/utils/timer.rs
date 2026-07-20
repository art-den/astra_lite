use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

pub struct Timer {
    thread:    Option<std::thread::JoinHandle<()>>,
    commands:  Arc<Mutex<Vec<TimerCommand>>>,
    exit_flag: Arc<AtomicBool>,
}

struct TimerCommand {
    fun:       Option<Arc<dyn Fn() + Sync + Send + 'static>>,
    time:      std::time::Instant,
    period_ms: u32,
    periodic:  bool,
}

impl Drop for Timer {
    fn drop(&mut self) {
        log::info!("Stopping Timer thread...");
        self.exit_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            _ = thread.join();
        }
        log::info!("Done!");
    }
}

impl Timer {
    pub fn new() -> Self {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let exit_flag = Arc::new(AtomicBool::new(false));

        let thread = {
            let commands = Arc::clone(&commands);
            let exit_flag = Arc::clone(&exit_flag);
            std::thread::spawn(move || {
                Self::thread_fun(&commands, &exit_flag);
            })
        };
        Self {
            thread: Some(thread),
            commands,
            exit_flag,
        }
    }

    pub fn exec(
        &self,
        period_ms: u32,
        periodic:  bool,
        fun:       impl Fn() + Sync + Send + 'static
    ) {
        let mut commands = self.commands.lock().unwrap();
        let command = TimerCommand {
            fun: Some(Arc::new(fun)),
            time: std::time::Instant::now(),
            period_ms,
            periodic,
        };
        commands.push(command);
    }

    pub fn clear(&self) {
        let mut commands = self.commands.lock().unwrap();
        commands.clear();
    }

    fn thread_fun(
        commands:  &Mutex<Vec<TimerCommand>>,
        exit_flag: &AtomicBool
    ) {
        while !exit_flag.load(Ordering::Relaxed) {
            let mut to_execute = Vec::new();
            {
                let mut commands = commands.lock().unwrap();
                for cmd in commands.iter_mut() {
                    if cmd.time.elapsed().as_millis() as u32 >= cmd.period_ms {
                        if let Some(f) = &cmd.fun {
                            to_execute.push(Arc::clone(f));
                            if cmd.periodic {
                                cmd.time = std::time::Instant::now();
                            } else {
                                cmd.fun = None;
                            }
                        }
                    }
                }
                commands.retain(|cmd| cmd.fun.is_some());
            }

            for f in to_execute {
                f();
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

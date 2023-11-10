use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

pub struct Timer {
    thread:    Option<std::thread::JoinHandle<()>>,
    commands:  Arc<Mutex<Vec<TimerCommand>>>,
    exit_flag: Arc<AtomicBool>,
}

struct TimerCommand {
    fun: Option<Box<dyn Fn() + Sync + Send + 'static>>,
    time: std::time::Instant,
    to_ms: u32,
    periodic: bool,
}

impl Drop for Timer {
    fn drop(&mut self) {
        log::info!("Stopping ThreadTimer thread...");
        self.exit_flag.store(true, Ordering::Relaxed);
        let thread = self.thread.take().unwrap();
        _ = thread.join();
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

    pub fn exec(&self, to_ms: u32, periodic: bool, fun: impl Fn() + Sync + Send + 'static) {
        let mut commands = self.commands.lock().unwrap();
        let command = TimerCommand {
            fun: Some(Box::new(fun)),
            time: std::time::Instant::now(),
            to_ms,
            periodic,
        };
        commands.push(command);
    }

    fn thread_fun(
        commands:  &Mutex<Vec<TimerCommand>>,
        exit_flag: &AtomicBool
    ) {
        while !exit_flag.load(Ordering::Relaxed) {
            let mut commands = commands.lock().unwrap();
            for cmd in &mut *commands {
                if cmd.time.elapsed().as_millis() as u32 >= cmd.to_ms {
                    if let Some(fun) = &mut cmd.fun {
                        fun();
                    }
                    if cmd.periodic {
                        cmd.time = std::time::Instant::now();
                    } else {
                        cmd.fun = None;
                    }
                }
            }
            commands.retain(|cmd| cmd.fun.is_some());
            drop(commands);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

use std::time::{Duration, Instant};

const UPDATE_INTERVAL: Duration = Duration::new(0, 200000000); // 5Hz

/// Keeps track of time, only updates if some time has passed since last time
pub struct Patience {
    last_time: Option<Instant>,
}

impl Patience {
    pub fn new() -> Patience {
        Patience { last_time: None }
    }
    pub fn have_been_patient(&mut self) -> bool {
        let now = Instant::now();
        match self.last_time {
            None => {
                self.last_time = Some(now);
                true
            }
            Some(mut last_time) => {
                if now < last_time {
                    // ??!?!
                    self.last_time = Some(now);
                    true
                } else {
                    let diff = now - last_time;
                    if diff >= UPDATE_INTERVAL * 5 {
                        self.last_time = Some(now);
                        true
                    } else if diff >= UPDATE_INTERVAL {
                        while last_time < now {
                            last_time += UPDATE_INTERVAL;
                        }
                        self.last_time = Some(last_time);
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }
}

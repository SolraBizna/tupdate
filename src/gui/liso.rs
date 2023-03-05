use std::{
    mem::swap,
};

use ::liso::{Color, InputOutput, Response, liso};

use super::*;

/// An interactive-capable, Liso-based "GUI". Suitable for use in piped
/// contexts as well.
pub struct LisoGui {
    io: Option<InputOutput>,
    last_task_output: String,
    last_subtask_output: String,
    last_progress_output: Option<(u16,u16)>,
}

/// True if we should pause after outputting a message or error, false if we
/// should not.
const SHOULD_PAUSE: bool = cfg!(any(feature="always_pause",all(windows,not(unix))));

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Consume {
    /// Consume everything. Return nothing.
    All,
    /// We are a "press enter to continue" prompt. Display that prompt, return
    /// when enter is pressed, clean up after.
    EnterToContinue,
    /// We are a "press enter to continue, or control-C to cancel" prompt.
    Proceed,
}

impl Gui for LisoGui {
    fn set_progress(&mut self, task: &str, subtask: &str, progress: Option<f32>) {
        let progress_output = progress.map(|ratio| {
            let term_width = terminal_size::terminal_size().map(|(w,_h)| w.0).unwrap_or(80);
            ((ratio.clamp(0.0, 1.0) * term_width as f32).floor() as u16, term_width)
        });
        if self.last_task_output == task && self.last_subtask_output == subtask && self.last_progress_output == progress_output {
            return; // nothing to be done
        }
        let mut line = liso!(+bold, task, -bold);
        if subtask != "" {
            line.add_text("\n");
            line.add_text(subtask);
        }
        if let Some((fill, width)) = progress_output.clone() {
            line.add_text("\n");
            if fill != 0 {
                line.set_colors(Some(Color::Cyan), Some(Color::Cyan));
                for _ in 0 .. fill {
                    line.add_text("=");
                }
            }
            if fill != width {
                line.set_colors(Some(Color::Black), Some(Color::Black));
                for _ in fill .. width {
                    line.add_text(" ");
                }
            }
            line.set_colors(None, None);
        }
        self.io.as_mut().unwrap().status(Some(line));
        if self.last_task_output != task {
            self.last_task_output = task.to_string();
        }
        if self.last_subtask_output != subtask {
            self.last_subtask_output = subtask.to_string();
        }
        self.last_progress_output = progress_output;
        self.consume_liso(Consume::All);
    }
    fn do_message(&mut self, title: &str, message: &str) {
        if SHOULD_PAUSE {
            let last_progress = self.take_progress();
            self.io.as_mut().unwrap().wrapln(liso!(+bold, fg=green, title));
            self.io.as_mut().unwrap().wrapln(message);
            self.consume_liso(Consume::EnterToContinue);
            self.restore_progress(last_progress);
        }
        else {
            self.io.as_mut().unwrap().wrapln(liso!(+bold, fg=green, title));
            self.io.as_mut().unwrap().wrapln(message);
        }
    }
    fn do_warning(&mut self, title: &str, message: &str, can_cancel: bool) -> bool {
        let last_progress = self.take_progress();
        self.io.as_mut().unwrap().wrapln(liso!(+bold, fg=yellow, title));
        self.io.as_mut().unwrap().wrapln(message);
        let ret = self.consume_liso(if can_cancel { Consume::Proceed } else { Consume::EnterToContinue }).is_some();
        self.restore_progress(last_progress);
        ret
    }
    fn do_error(&mut self, title: &str, message: &str) {
        if SHOULD_PAUSE {
            let last_progress = self.take_progress();
            self.io.as_mut().unwrap().wrapln(liso!(+bold, fg=red, title));
            self.io.as_mut().unwrap().wrapln(message);
            self.consume_liso(Consume::EnterToContinue);
            self.restore_progress(last_progress);
        }
        else {
            self.io.as_mut().unwrap().wrapln(liso!(+bold, fg=red, title));
            self.io.as_mut().unwrap().wrapln(message);
        }
    }
    fn verbose(&mut self, message: &str) {
        self.io.as_mut().unwrap().wrapln(liso!(dim, fg=cyan, message));
    }
}

impl LisoGui {
    pub fn go<T: FnOnce(Rc<RefCell<dyn Gui>>) -> ExitCode + Send + Sync + 'static>(f: T) -> Result<ExitCode, T> {
        let io = InputOutput::new();
        io.prompt("", false, true);
        Ok(f(Rc::new(RefCell::new(LisoGui {
            io: Some(io),
            last_task_output: String::new(),
            last_subtask_output: String::new(),
            last_progress_output: None,
        }))))
    }
    fn take_progress(&mut self) -> (String, String, Option<(u16,u16)>) {
        let (mut last_task_output, mut last_subtask_output, last_progress_output)
        = (String::new(), String::new(), self.last_progress_output.take());
        swap(&mut last_task_output, &mut self.last_task_output);
        swap(&mut last_subtask_output, &mut self.last_subtask_output);
        self.io.as_mut().unwrap().status::<&str>(None);
        (last_task_output, last_subtask_output, last_progress_output)
    }
    fn consume_liso(&mut self, mode: Consume) -> Option<String> {
        let mut ret = None;
        match mode {
            Consume::All => {
                while let Some(response) = self.io.as_mut().unwrap().try_read() {
                    match response {
                        Response::Dead => std::process::exit(1),
                        _ => (),
                    }
                }
            },
            Consume::EnterToContinue | Consume::Proceed => {
                let prompt_text = match mode {
                    Consume::EnterToContinue => "(press enter to continue)\n",
                    Consume::Proceed => "(press enter to continue, or control-C to cancel)\n",
                    _ => unreachable!(),
                };
                self.io.as_mut().unwrap().prompt(liso!(dim, prompt_text, -dim), true, true);
                // workaround for blocking_recv being disallowed in an async context
                let mut io = self.io.take().unwrap();
                let result = std::thread::spawn(move || {
                    let ret;
                    loop {
                        let response = io.read_blocking();
                        match response {
                            Response::Input(x) => {
                                ret = Some(x);
                                break;
                            },
                            Response::Dead => std::process::exit(1),
                            Response::Quit | Response::Finish if mode == Consume::Proceed => {
                                ret = None;
                                break;
                            },
                            _ => (),
                        }
                    }
                    (io, ret)
                }).join().unwrap();
                self.io = Some(result.0);
                ret = result.1;
                self.io.as_mut().unwrap().prompt("", false, false);
            },
        }
        ret
    }
    fn restore_progress(&mut self, last: (String, String, Option<(u16,u16)>)) {
        if last.0 != "" || last.1 != "" || last.2 != None {
            self.set_progress(
                &last.0,
                &last.1,
                last.2.map(|(n,d)| {
                    n as f32 / d as f32
                })
            );
        }
    }
}

impl Drop for LisoGui {
    fn drop(&mut self) {
        self.io.as_mut().unwrap().status::<&str>(None);
        self.io.as_mut().unwrap().prompt("", false, true);
        self.io.as_mut().unwrap().send_custom(());
        loop {
            let response = self.io.as_mut().unwrap().try_read();
            match response {
                Some(Response::Dead) => std::process::exit(1),
                Some(Response::Custom(_)) => break,
                _ => (),
            }
        }
    }
}
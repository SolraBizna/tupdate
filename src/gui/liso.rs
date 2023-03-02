use std::{
    mem::swap,
};

use ::liso::{Color, InputOutput, Response, liso};

use super::*;

/// An interactive-capable, Liso-based "GUI". Suitable for use in piped
/// contexts as well.
pub struct LisoGui {
    io: InputOutput,
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
        self.io.status(Some(line));
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
            self.io.wrapln(liso!(+bold, fg=green, title));
            self.io.wrapln(message);
            self.consume_liso(Consume::EnterToContinue);
            self.restore_progress(last_progress);
        }
        else {
            self.io.wrapln(liso!(+bold, fg=green, title));
            self.io.wrapln(message);
        }
    }
    fn do_warning(&mut self, title: &str, message: &str, _can_cancel: bool) -> bool {
        let last_progress = self.take_progress();
        self.io.wrapln(liso!(+bold, fg=yellow, title));
        self.io.wrapln(message);
        let ret = self.consume_liso(Consume::EnterToContinue).is_some();
        self.restore_progress(last_progress);
        ret
    }
    fn do_error(&mut self, title: &str, message: &str) {
        if SHOULD_PAUSE {
            let last_progress = self.take_progress();
            self.io.wrapln(liso!(+bold, fg=red, title));
            self.io.wrapln(message);
            self.consume_liso(Consume::EnterToContinue);
            self.restore_progress(last_progress);
        }
        else {
            self.io.wrapln(liso!(+bold, fg=red, title));
            self.io.wrapln(message);
        }
    }
    fn verbose(&mut self, message: &str) {
        self.io.wrapln(liso!(dim, fg=cyan, message));
    }
}

impl LisoGui {
    pub fn new() -> Option<Rc<RefCell<dyn Gui>>> {
        let io = InputOutput::new();
        io.prompt("", false, true);
        Some(Rc::new(RefCell::new(LisoGui {
            io,
            last_task_output: String::new(),
            last_subtask_output: String::new(),
            last_progress_output: None,
        })))
    }
    fn take_progress(&mut self) -> (String, String, Option<(u16,u16)>) {
        let (mut last_task_output, mut last_subtask_output, last_progress_output)
        = (String::new(), String::new(), self.last_progress_output.take());
        swap(&mut last_task_output, &mut self.last_task_output);
        swap(&mut last_subtask_output, &mut self.last_subtask_output);
        self.io.status::<&str>(None);
        (last_task_output, last_subtask_output, last_progress_output)
    }
    fn consume_liso(&mut self, mode: Consume) -> Option<String> {
        let mut ret = None;
        match mode {
            Consume::All => {
                while let Some(response) = self.io.try_read() {
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
                self.io.prompt(liso!(dim, prompt_text, -dim), true, true);
                loop {
                    let response = self.io.read_blocking();
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
                self.io.prompt("", false, false);
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
        self.io.status::<&str>(None);
        self.io.prompt("", false, true);
        self.io.send_custom(());
        loop {
            let response = self.io.try_read();
            match response {
                Some(Response::Dead) => std::process::exit(1),
                Some(Response::Custom(_)) => break,
                _ => (),
            }
        }
    }
}
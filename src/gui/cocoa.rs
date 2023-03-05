use super::*;

use std::{
    process::ExitCode,
    sync::{Mutex, mpsc},
};

use cacao::{
    appkit::{Alert, App, AppDelegate, window::{Window, WindowConfig, WindowDelegate, WindowStyle}},
    layout::{Layout, LayoutConstraint},
    progress::ProgressIndicator,
    text::{Label, TextAlign},
    view::View, notification_center::Dispatcher,
};

struct GuiApp {
    window: Mutex<Option<Window<GuiWindow>>>,
    res_tx: mpsc::Sender<bool>,
}

#[derive(Default)]
struct GuiWindow {
    view: View,
    tasklabel: Label,
    subtasklabel: Label,
    bar: ProgressIndicator,
    determinate: bool,
}

const TOP_GAP: f64 = 16.0;
const BAR_GAP: f64 = 12.0;
const HGAP: f64 = 24.0;

impl AppDelegate for GuiApp {
    fn did_finish_launching(&self) {
        App::activate();
        let mut winlock = self.window.lock().unwrap();
        let mut config = WindowConfig::default();
        config.set_styles(&[
            WindowStyle::Titled,
            WindowStyle::Miniaturizable,
        ]);
        *winlock = Some(Window::with(config, GuiWindow::default()));
        winlock.as_ref().unwrap().show();
    }
}

impl Dispatcher for GuiApp {
    type Message = Request;
    fn on_ui_message(&self, message: Self::Message) {
        let mut window = self.window.lock().unwrap();
        let window = window.as_mut().unwrap();
        let windel = window.delegate.as_mut().unwrap();
        match message {
            Request::SetProgress { task, subtask, progress } => {
                if progress.is_none() && windel.determinate {
                    windel.bar.set_indeterminate(true);
                    windel.bar.start_animation();
                }
                else if let Some(progress) = progress {
                    if !windel.determinate {
                        windel.bar.stop_animation();
                        windel.bar.set_indeterminate(false);
                    }
                    windel.bar.set_value(progress as f64 * 100.0);
                }
                windel.determinate = progress.is_some();
                if task != windel.tasklabel.get_text() {
                    windel.tasklabel.set_text(task);
                }
                if subtask != windel.subtasklabel.get_text() {
                    windel.subtasklabel.set_text(subtask);
                }
            },
            // TODO: cancellable Warning
            Request::Message { title, message}
            | Request::Warning { title, message, .. }
            | Request::Error { title, message } => {
                window.close();
                let alert = Alert::new(&title, &message);
                alert.show();
                window.show();
                let _ = self.res_tx.send(true);
            },
        }
    }
}

impl WindowDelegate for GuiWindow {
    const NAME: &'static str = "GuiApp";
    fn did_load(&mut self, window: Window) {
        window.set_title("Tejat Updater");
        self.tasklabel.set_text("Initializing...");
        self.subtasklabel.set_text_alignment(TextAlign::Right);
        self.bar.set_indeterminate(true);
        self.bar.start_animation();
        self.view.add_subview(&self.tasklabel);
        self.view.add_subview(&self.subtasklabel);
        self.view.add_subview(&self.bar);
        LayoutConstraint::activate(&[
            self.view.width.constraint_equal_to_constant(512.0),
            self.tasklabel.top.constraint_equal_to(&self.view.top).offset(TOP_GAP),
            self.tasklabel.leading.constraint_equal_to(&self.view.leading).offset(HGAP),
            self.tasklabel.trailing.constraint_equal_to(&self.view.trailing).offset(-HGAP),
            self.subtasklabel.top.constraint_equal_to(&self.view.top).offset(TOP_GAP),
            self.subtasklabel.leading.constraint_equal_to(&self.view.leading).offset(HGAP),
            self.subtasklabel.trailing.constraint_equal_to(&self.view.trailing).offset(-HGAP),
            self.bar.top.constraint_equal_to(&self.subtasklabel.bottom).offset(BAR_GAP),
            self.bar.leading.constraint_equal_to(&self.view.leading).offset(HGAP),
            self.bar.trailing.constraint_equal_to(&self.view.trailing).offset(-HGAP),
            self.view.bottom.constraint_equal_to(&self.bar.bottom).offset(BAR_GAP),
        ]);
        window.set_content_view(&self.view);
    }
}

#[derive(Debug)]
enum Request {
    SetProgress { task: String, subtask: String, progress: Option<f32> },
    Message { title: String, message: String },
    Warning { title: String, message: String, #[allow(dead_code)] can_cancel: bool },
    Error { title: String, message: String },
}

pub struct CocoaGui {
    res_rx: mpsc::Receiver<bool>,
}

impl CocoaGui {
    pub fn go<T: FnOnce(Rc<RefCell<dyn Gui>>) -> ExitCode + Send + Sync + 'static>(f: T) -> Result<ExitCode, T> {
        let (res_tx, res_rx) = mpsc::channel();
        std::thread::spawn(move || {
            f(Rc::new(RefCell::new(CocoaGui { res_rx })));
            App::terminate();
        });
        App::new("net.tejat.tupdate", GuiApp {
            res_tx,
            window: Mutex::new(None),
        }).run();
        Ok(ExitCode::SUCCESS)
    }
}

impl Gui for CocoaGui {
    fn set_progress(&mut self, task: &str, subtask: &str, progress: Option<f32>) {
        App::<GuiApp, Request>::dispatch_main(Request::SetProgress { task: task.to_string(), subtask: subtask.to_string(), progress });
    }
    fn do_message(&mut self, title: &str, message: &str) {
        App::<GuiApp, Request>::dispatch_main(Request::Message { title: title.to_string(), message: message.to_string() });
        self.res_rx.recv().unwrap();
    }
    fn do_warning(&mut self, title: &str, message: &str, can_cancel: bool) -> bool {
        App::<GuiApp, Request>::dispatch_main(Request::Warning { title: title.to_string(), message: message.to_string(), can_cancel });
        self.res_rx.recv().unwrap()
    }
    fn do_error(&mut self, title: &str, message: &str) {
        App::<GuiApp, Request>::dispatch_main(Request::Error { title: title.to_string(), message: message.to_string() });
        self.res_rx.recv().unwrap();
    }
}
use super::*;

pub struct BatchGui;

impl Gui for BatchGui {
    fn set_progress(
        &mut self,
        _task: &str,
        _subtask: &str,
        _progress: Option<f32>,
    ) {
    }
    fn do_message(&mut self, _title: &str, message: &str) {
        println!(": {}", message);
    }
    fn do_warning(
        &mut self,
        _title: &str,
        message: &str,
        _can_cancel: bool,
    ) -> bool {
        println!("? {}", message);
        true
    }
    fn do_error(&mut self, _title: &str, message: &str) {
        println!("! {}", message);
    }
}

impl BatchGui {
    pub fn go<
        T: FnOnce(Rc<RefCell<dyn Gui>>) -> ExitCode + Send + Sync + 'static,
    >(
        _: Option<bool>,
        f: T,
    ) -> Result<ExitCode, T> {
        Ok(f(Rc::new(RefCell::new(BatchGui))))
    }
}

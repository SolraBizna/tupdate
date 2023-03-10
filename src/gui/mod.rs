use std::{
    cell::RefCell,
    process::ExitCode,
    rc::Rc,
};

mod batch;
#[cfg(feature="liso")]
mod liso;
#[cfg(target_os="macos")]
mod cocoa;

/// A graphical front end for Tupdate.
pub trait Gui: Send {
    /// With the GUI window up, establish the given progress bar and status
    /// messages. Submessage may not be displayed on some GUIs. Any `do_*` call
    /// may temporarily hide the progress window, but if this is done, the
    /// existing progress display must be restored.
    fn set_progress(&mut self, task: &str, subtask: &str, progress: Option<f32>);
    /// Display a message, with an OK button. Return after display. Title not
    /// displayed on all GUIs.
    fn do_message(&mut self, title: &str, message: &str);
    /// Display a warning, with an OK button and an optional Cancel button.
    /// Returns true if OK was pressed. Title not displayed on all GUIs.
    fn do_warning(&mut self, title: &str, message: &str, can_cancel: bool) -> bool;
    /// Display an error, with an OK button. Return after display. Title not
    /// displayed on all GUIs.
    fn do_error(&mut self, title: &str, message: &str);
    /// Do "verbose output" to stderr or stdout or system log or etc.
    fn verbose(&mut self, message: &str) {
        eprintln!("{}", message);
    }
}

/// Tries to make a new GUI and use it to run the given function. Returns an
/// `ExitCode`.
pub fn run_gui<T: FnOnce(Rc<RefCell<dyn Gui>>) -> ExitCode + Send + Sync + 'static>(mut target_gui: Option<String>, pause: Option<bool>, f: T) -> ExitCode {
    if target_gui.as_ref().map(String::as_str) == Some("help") {
        println!("Available GUIs:");
        println!("    batch: No progress information. Outputs all messages directly to stdout. Assumes \"OK\" on all prompts.");
        if cfg!(target_os="macos") {
            println!("    cocoa: Full Macintosh GUI.");
        }
        if cfg!(feature="gui_liso") {
            println!("    liso: Interactive terminal experience. Pipe friendly. (Used by default if all three standard file descriptors are for an interactive terminal.)");
        }
        return ExitCode::SUCCESS;
    }
    #[cfg(feature="gui_liso")]
    if target_gui == None && atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stdout) && atty::is(atty::Stream::Stderr) {
        // If we are being run in an interactive terminal, and no --gui option
        // was specified, assume that a terminal-based UI is desired.
        target_gui = Some("liso".to_string());
    }
    if let Some(target_gui) = target_gui {
        match target_gui.as_str() {
            "batch" => return batch::BatchGui::go(pause, f).unwrap_or(ExitCode::FAILURE),
            #[cfg(target_os="macos")]
            "cocoa" => return cocoa::CocoaGui::go(pause, f).unwrap_or(ExitCode::FAILURE),
            #[cfg(feature="gui_liso")]
            "liso" => return liso::LisoGui::go(pause, f).unwrap_or(ExitCode::FAILURE),
            _ => {
                eprintln!("The GUI type you requested is unknown or unavailable. Try \"--gui help\".");
                return ExitCode::FAILURE
            },
        }
    }
    #[cfg(target_os="macos")]
    let f = match cocoa::CocoaGui::go(pause, f) {
        Ok(x) => return x,
        Err(x) => x,
    };
    // Wayland or X GUIs would go here
    #[cfg(feature="gui_liso")]
    let f = match liso::LisoGui::go(pause, f) {
        Ok(x) => return x,
        Err(x) => x,
    };
    let _f = match batch::BatchGui::go(pause, f) {
        Ok(x) => return x,
        Err(x) => x,
    };
    panic!("No GUI could be started—this should never happen!")
}
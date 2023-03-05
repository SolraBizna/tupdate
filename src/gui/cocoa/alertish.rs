//! Modified version of the NSAlert wrapper from `cacao`.

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use objc_id::Id;

use cacao::foundation::{id, NSInteger, NSString};

#[derive(Debug)]
pub struct Alert(Id<Object>);

pub enum AlertStyle {
    Warning, Informational, Error
}

impl Alert {
    pub fn new(title: &str, message: &str, can_cancel: bool, alert_type: AlertStyle) -> Self {
        let title = NSString::new(title);
        let message = NSString::new(message);
        let ok = NSString::new("OK");
        let alert_style = match alert_type {
            AlertStyle::Warning => 0,
            AlertStyle::Informational => 1, 
            AlertStyle::Error => 2,
        };
        Alert(unsafe {
            let alert: id = msg_send![class!(NSAlert), new];
            let _: () = msg_send![alert, setMessageText: title];
            let _: () = msg_send![alert, setInformativeText: message];
            let _: () = msg_send![alert, addButtonWithTitle: ok];
            if can_cancel {
                let _: () = msg_send![alert, addButtonWithTitle: NSString::new("Cancel")];
            }
            let _: () = msg_send![alert, setAlertStyle: alert_style];
            Id::from_ptr(alert)
        })
    }

    /// Shows this alert as a modal, and return the response. 1000 = OK, 1001 = cancel.
    pub fn run_modal(&self) -> NSInteger {
        unsafe {
            msg_send![&*self.0, runModal]
        }
    }
}


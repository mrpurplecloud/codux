use crate::terminal_pty::TerminalPtyConfig;
use codux_terminal_core::TerminalEvent;
use serde_json::Value;
use std::sync::Arc;

pub type RuntimeTerminalOutputForwarder = Box<dyn Fn(Vec<u8>) + Send + Sync>;
pub type RuntimeTerminalEventForwarder = Box<dyn Fn(TerminalEvent) + Send + Sync>;
#[derive(Clone, Debug)]
pub enum RuntimeTerminalStreamEvent {
    Output(Vec<u8>),
    Replace {
        history: Vec<u8>,
        screen: Vec<u8>,
        cols: usize,
        rows: usize,
    },
    Event(TerminalEvent),
}
pub type RuntimeTerminalStreamForwarder = Arc<dyn Fn(RuntimeTerminalStreamEvent) + Send + Sync>;

pub trait RuntimeTerminalController: Send + Sync {
    fn open_terminal(&self, config: &TerminalPtyConfig) -> Result<String, String>;
    fn list_terminals(&self) -> Result<Value, String>;
    fn terminal_input(&self, session_id: &str, bytes: &[u8]) -> bool;
    fn terminal_resize(&self, session_id: &str, cols: u16, rows: u16) -> bool;
    fn close_terminal(&self, session_id: &str) -> Result<(), String>;
    fn close_terminal_fire(&self, session_id: &str) -> bool;
    fn register_terminal_output(&self, session_id: &str, forwarder: RuntimeTerminalOutputForwarder);
    fn unregister_terminal_output(&self, session_id: &str);
    fn register_terminal_events(
        &self,
        _session_id: &str,
        _forwarder: RuntimeTerminalEventForwarder,
    ) {
    }
    fn unregister_terminal_events(&self, _session_id: &str) {}
    fn register_terminal_stream(
        &self,
        session_id: &str,
        forwarder: RuntimeTerminalStreamForwarder,
    ) {
        let output_forwarder = Arc::clone(&forwarder);
        self.register_terminal_output(
            session_id,
            Box::new(move |bytes| output_forwarder(RuntimeTerminalStreamEvent::Output(bytes))),
        );
        self.register_terminal_events(
            session_id,
            Box::new(move |event| forwarder(RuntimeTerminalStreamEvent::Event(event))),
        );
    }
    fn unregister_terminal_stream(&self, session_id: &str) {
        self.unregister_terminal_output(session_id);
        self.unregister_terminal_events(session_id);
    }
    fn terminal_viewport_identity(&self) -> Option<String> {
        None
    }
    fn terminal_viewport_auto(&self, _session_id: &str) -> bool {
        false
    }
    fn terminal_viewport_force(&self, _session_id: &str) -> bool {
        false
    }
    fn terminal_viewport_renew(&self, _session_id: &str) -> bool {
        false
    }
    fn terminal_viewport_release(&self, _session_id: &str) -> bool {
        false
    }
    fn terminal_buffer_tail(&self, _session_id: &str) -> bool {
        false
    }
}

impl RuntimeTerminalController for crate::remote::RemoteController {
    fn open_terminal(&self, config: &TerminalPtyConfig) -> Result<String, String> {
        crate::remote::RemoteController::open_terminal(self, config)
    }

    fn list_terminals(&self) -> Result<Value, String> {
        crate::remote::RemoteController::list_terminals(self)
    }

    fn terminal_input(&self, session_id: &str, bytes: &[u8]) -> bool {
        crate::remote::RemoteController::terminal_input(
            self,
            session_id,
            &String::from_utf8_lossy(bytes),
        )
    }

    fn terminal_resize(&self, session_id: &str, cols: u16, rows: u16) -> bool {
        crate::remote::RemoteController::terminal_resize(self, session_id, cols, rows)
    }

    fn close_terminal(&self, session_id: &str) -> Result<(), String> {
        crate::remote::RemoteController::close_terminal(self, session_id).map(|_| ())
    }

    fn close_terminal_fire(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::close_terminal_fire(self, session_id)
    }

    fn register_terminal_output(
        &self,
        session_id: &str,
        forwarder: RuntimeTerminalOutputForwarder,
    ) {
        crate::remote::RemoteController::register_terminal_output(self, session_id, forwarder);
    }

    fn unregister_terminal_output(&self, session_id: &str) {
        crate::remote::RemoteController::unregister_terminal_output(self, session_id);
    }

    fn register_terminal_stream(
        &self,
        session_id: &str,
        forwarder: RuntimeTerminalStreamForwarder,
    ) {
        crate::remote::RemoteController::register_terminal_stream(self, session_id, forwarder);
    }

    fn unregister_terminal_stream(&self, session_id: &str) {
        crate::remote::RemoteController::unregister_terminal_stream(self, session_id);
    }

    fn terminal_viewport_identity(&self) -> Option<String> {
        Some(crate::remote::RemoteController::terminal_viewport_identity(
            self,
        ))
    }

    fn terminal_viewport_auto(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::terminal_viewport_auto(self, session_id)
    }

    fn terminal_viewport_force(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::terminal_viewport_force(self, session_id)
    }

    fn terminal_viewport_renew(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::terminal_viewport_renew(self, session_id)
    }

    fn terminal_viewport_release(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::terminal_viewport_release(self, session_id)
    }

    fn terminal_buffer_tail(&self, session_id: &str) -> bool {
        crate::remote::RemoteController::terminal_buffer_tail(self, session_id)
    }
}

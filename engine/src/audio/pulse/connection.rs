//! Blocking client connection to a PulseAudio-protocol server: pipewire-pulse on PipeWire, or PulseAudio.
//! The non-threaded main loop is pumped by each call until its operation completes, so the calls run
//! on the control thread only, never on an audio thread.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{anyhow, bail, Result};
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OperationState};
use libpulse_binding::proplist::{properties, Proplist};

/// One sink or source as the server reports it. `pulse_name` is what connections use; `description`
/// is the UI name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub pulse_name: String,
    pub description: String,
    pub rate: u32,
    pub owner_module: Option<u32>,
    pub is_monitor: bool,
    pub is_default: bool,
}

/// Server identity and the default sink and source names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub server_name: String,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

/// Field order matters: the context must be released before the main loop it is bound to, and
/// `Drop` disconnects first, so the server sees an orderly close.
pub struct Server {
    context: Context,
    mainloop: Mainloop,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

impl Server {
    /// Connects to the default server without autospawning a PulseAudio daemon.
    pub fn connect() -> Result<Self> {
        let mut mainloop = Mainloop::new().ok_or_else(|| anyhow!("cannot create the PulseAudio main loop"))?;
        let mut proplist = Proplist::new().ok_or_else(|| anyhow!("cannot create a PulseAudio property list"))?;
        proplist
            .set_str(properties::APPLICATION_NAME, "HushCore")
            .map_err(|_| anyhow!("cannot set the application name"))?;
        let mut context = Context::new_with_proplist(&mainloop, "HushCore", &proplist)
            .ok_or_else(|| anyhow!("cannot create a PulseAudio context"))?;
        context
            .connect(None, FlagSet::NOAUTOSPAWN, None)
            .map_err(|e| anyhow!("no PulseAudio or PipeWire server reachable: {e}"))?;
        loop {
            match context.get_state() {
                State::Ready => break,
                State::Failed | State::Terminated => bail!("PulseAudio connection failed"),
                _ => {}
            }
            pump(&mut mainloop)?;
        }
        Ok(Self { context, mainloop })
    }

    fn wait<T: ?Sized>(&mut self, op: Operation<T>) -> Result<()> {
        while op.get_state() == OperationState::Running {
            pump(&mut self.mainloop)?;
        }
        Ok(())
    }

    /// Server name and the default sink and source, in one round trip.
    pub fn server_info(&mut self) -> Result<ServerInfo> {
        let slot = Rc::new(RefCell::new(None::<ServerInfo>));
        let sink = slot.clone();
        let op = self.context.introspect().get_server_info(move |info| {
            *sink.borrow_mut() = Some(ServerInfo {
                server_name: info.server_name.as_deref().unwrap_or_default().to_string(),
                default_sink: info.default_sink_name.as_deref().map(str::to_string),
                default_source: info.default_source_name.as_deref().map(str::to_string),
            });
        });
        self.wait(op)?;
        let found = slot.borrow_mut().take();
        found.ok_or_else(|| anyhow!("server returned no server info"))
    }

    /// Every sink, with the server default marked.
    pub fn sinks(&mut self, defaults: &ServerInfo) -> Result<Vec<Node>> {
        let rows = Rc::new(RefCell::new(Vec::new()));
        let out = rows.clone();
        let default = defaults.default_sink.clone();
        let op = self.context.introspect().get_sink_info_list(move |item| {
            if let ListResult::Item(info) = item {
                let pulse_name = info.name.as_deref().unwrap_or_default().to_string();
                out.borrow_mut().push(Node {
                    is_default: default.as_deref() == Some(pulse_name.as_str()),
                    description: info.description.as_deref().unwrap_or(&pulse_name).to_string(),
                    pulse_name,
                    rate: info.sample_spec.rate,
                    owner_module: info.owner_module,
                    is_monitor: false,
                });
            }
        });
        self.wait(op)?;
        Ok(rows.take())
    }

    /// Every source, monitors included (`is_monitor`), with the server default marked.
    pub fn sources(&mut self, defaults: &ServerInfo) -> Result<Vec<Node>> {
        let rows = Rc::new(RefCell::new(Vec::new()));
        let out = rows.clone();
        let default = defaults.default_source.clone();
        let op = self.context.introspect().get_source_info_list(move |item| {
            if let ListResult::Item(info) = item {
                let pulse_name = info.name.as_deref().unwrap_or_default().to_string();
                out.borrow_mut().push(Node {
                    is_default: default.as_deref() == Some(pulse_name.as_str()),
                    description: info.description.as_deref().unwrap_or(&pulse_name).to_string(),
                    pulse_name,
                    rate: info.sample_spec.rate,
                    owner_module: info.owner_module,
                    is_monitor: info.monitor_of_sink.is_some(),
                });
            }
        });
        self.wait(op)?;
        Ok(rows.take())
    }

    /// Loads a server module and returns its index. The server reports failure as `u32::MAX`.
    pub fn load_module(&mut self, module: &str, args: &str) -> Result<u32> {
        let slot = Rc::new(RefCell::new(u32::MAX));
        let out = slot.clone();
        let op = self.context.introspect().load_module(module, args, move |index| {
            *out.borrow_mut() = index;
        });
        self.wait(op)?;
        let index = *slot.borrow();
        if index == u32::MAX {
            bail!("the server refused to load {module} with \"{args}\"");
        }
        Ok(index)
    }

    /// Unloads a module by index. Returns false when the server refused.
    pub fn unload_module(&mut self, index: u32) -> Result<bool> {
        let slot = Rc::new(RefCell::new(false));
        let out = slot.clone();
        let op = self.context.introspect().unload_module(index, move |ok| {
            *out.borrow_mut() = ok;
        });
        self.wait(op)?;
        let ok = *slot.borrow();
        Ok(ok)
    }
}

fn pump(mainloop: &mut Mainloop) -> Result<()> {
    match mainloop.iterate(true) {
        IterateResult::Success(_) => Ok(()),
        other => bail!("PulseAudio main loop stopped: {other:?}"),
    }
}

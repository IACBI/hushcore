//! The control loop's state machine: owns the live session, applies commands, and reopens the
//! devices with backoff when a session reports a lost device (unplug, invalidation, sample-rate
//! change, sleep/resume, audio service restart). Generic over the backend so it is unit-tested
//! without audio hardware.

use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{Controller, SessionLinks, StartRequest};
use crate::ipc::{self, Command, Event, Inbound, StartParams};

pub const RETRY_FIRST: Duration = Duration::from_millis(250);
pub const RETRY_MAX: Duration = Duration::from_secs(2);
/// A session must run this long before a later device loss starts the backoff from scratch;
/// otherwise a device that opens and fails at once would be retried every 250 ms forever.
pub const STABLE_AFTER: Duration = Duration::from_secs(2);

/// What the supervisor needs from a platform backend.
pub trait Backend {
    type Session;
    fn start(&mut self, req: &StartRequest, controller: &Arc<Controller>, links: &SessionLinks) -> Result<Self::Session>;
    fn set_monitor(&mut self, session: &mut Self::Session, enabled: bool, device: &str) -> Result<()>;
    /// Tear down and announce `stopped`.
    fn stop(&mut self, session: Self::Session);
    /// Tear down silently; a restart follows.
    fn abandon(&mut self, session: Self::Session);
    fn devices_event(&mut self) -> Result<Event>;
}

struct Retry {
    attempt: u32,
    due: Instant,
    /// The original failure ("input device lost"); retry errors are appended to it when reported.
    cause: String,
}

pub struct Supervisor<B: Backend> {
    backend: B,
    controller: Arc<Controller>,
    events: Sender<Event>,
    control: Sender<Inbound>,
    session: Option<B::Session>,
    session_id: u64,
    /// The last request that started successfully, with monitoring as currently set.
    last: Option<StartRequest>,
    retry: Option<Retry>,
    /// When the current session opened, and how many attempts it took to get there.
    opened_at: Option<Instant>,
    streak: u32,
}

/// Delay before retry `attempt` (1-based): 250 ms doubling, capped at 2 s.
pub fn backoff(attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(8);
    (RETRY_FIRST * factor).min(RETRY_MAX)
}

impl<B: Backend> Supervisor<B> {
    pub fn new(backend: B, controller: Arc<Controller>, events: Sender<Event>, control: Sender<Inbound>) -> Self {
        Self {
            backend,
            controller,
            events,
            control,
            session: None,
            session_id: 0,
            last: None,
            retry: None,
            opened_at: None,
            streak: 0,
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn error(&self, message: impl Into<String>) {
        self.emit(Event::Error { message: message.into(), fatal: false });
    }

    pub fn is_running(&self) -> bool {
        self.session.is_some()
    }

    pub fn is_reconnecting(&self) -> bool {
        self.retry.is_some()
    }

    /// When the control loop must wake up even without input.
    pub fn deadline(&self) -> Option<Instant> {
        self.retry.as_ref().map(|r| r.due)
    }

    pub fn handle(&mut self, command: Command) {
        match command {
            Command::Devices => match self.backend.devices_event() {
                Ok(event) => self.emit(event),
                Err(e) => self.error(format!("device enumeration failed: {e:#}")),
            },
            Command::Start(params) => self.start(params),
            Command::Settings { settings } => self.controller.set_settings(settings),
            Command::Monitor { enabled, device } => self.monitor(enabled, device),
            Command::Stop => self.stop(),
            Command::Quit => self.shutdown(),
        }
    }

    fn links(&mut self) -> SessionLinks {
        self.session_id += 1;
        SessionLinks { events: self.events.clone(), control: self.control.clone(), session_id: self.session_id }
    }

    fn open(&mut self, request: &StartRequest) -> Result<B::Session> {
        let links = self.links();
        let mut session = self.backend.start(request, &self.controller, &links)?;
        if request.monitor_enabled {
            if let Err(e) = self.backend.set_monitor(&mut session, true, &request.monitor) {
                self.error(format!("{e:#}"));
            }
        }
        Ok(session)
    }

    fn start(&mut self, params: StartParams) {
        self.retry = None;
        if let Some(session) = self.session.take() {
            self.backend.stop(session);
        }
        if let Some(settings) = params.settings {
            self.controller.set_settings(settings);
        }
        let mode = match ipc::parse_mode(&params.mode) {
            Ok(mode) => mode,
            Err(message) => return self.error(message),
        };
        let request = StartRequest {
            input: params.input,
            output: params.output,
            monitor: params.monitor,
            monitor_enabled: params.monitor_enabled,
            mode,
            mute_output: false,
        };
        match self.open(&request) {
            Ok(session) => {
                self.session = Some(session);
                self.last = Some(request);
                self.opened_at = Some(Instant::now());
                self.streak = 0;
            }
            Err(e) => {
                self.last = None;
                self.error(format!("could not start audio: {e:#}"));
            }
        }
    }

    fn monitor(&mut self, enabled: bool, device: String) {
        if let Some(last) = self.last.as_mut() {
            last.monitor_enabled = enabled;
            last.monitor = device.clone();
        }
        // Monitoring is part of `start` while stopped (or applied on reconnect); a stray monitor
        // command is not an error.
        let Some(session) = self.session.as_mut() else { return };
        match self.backend.set_monitor(session, enabled, &device) {
            Ok(()) => eprintln!("monitor: enabled={enabled} device=\"{device}\""),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Stops the session or a pending reconnect. Always leaves the backend with a `stopped` event.
    fn stop(&mut self) {
        self.retry = None;
        self.last = None;
        match self.session.take() {
            Some(session) => self.backend.stop(session),
            None => self.emit(Event::Stopped),
        }
    }

    /// Stops everything on `quit` or stdin EOF.
    pub fn shutdown(&mut self) {
        self.retry = None;
        if let Some(session) = self.session.take() {
            self.backend.stop(session);
        }
    }

    pub fn device_lost(&mut self, session: u64, reason: String, now: Instant) {
        if session != self.session_id || self.session.is_none() {
            return; // from a session that was already stopped or replaced
        }
        if let Some(old) = self.session.take() {
            self.backend.abandon(old);
        }
        if self.last.is_none() {
            self.emit(Event::Stopped);
            return;
        }
        let stable = self.opened_at.is_some_and(|t| now.saturating_duration_since(t) >= STABLE_AFTER);
        let attempt = if stable { 1 } else { self.streak + 1 };
        eprintln!("supervisor: {reason}; reconnecting");
        self.schedule(attempt, reason, None);
    }

    fn schedule(&mut self, attempt: u32, cause: String, last_error: Option<String>) {
        let delay = backoff(attempt);
        let reason = match last_error {
            Some(e) => format!("{cause}: {e}"),
            None => cause.clone(),
        };
        self.emit(Event::Reconnecting { reason, attempt, retry_in_ms: delay.as_millis() as u64 });
        self.retry = Some(Retry { attempt, due: Instant::now() + delay, cause });
    }

    /// Runs a due reconnect attempt. Call when `deadline()` has passed.
    pub fn tick(&mut self, now: Instant) {
        let Some(retry) = self.retry.as_ref() else { return };
        if now < retry.due {
            return;
        }
        let Retry { attempt, cause, .. } = self.retry.take().expect("checked above");
        let Some(request) = self.last.clone() else { return };
        match self.open(&request) {
            Ok(session) => {
                eprintln!("supervisor: reconnected after {attempt} attempt(s)");
                self.session = Some(session);
                self.opened_at = Some(now);
                self.streak = attempt;
            }
            Err(e) => self.schedule(attempt + 1, cause, Some(format!("{e:#}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    /// Fake backend: `fail_next` start attempts fail, then they succeed.
    #[derive(Default)]
    struct Fake {
        fail_next: u32,
        log: Arc<Mutex<Vec<String>>>,
        last_links: Option<SessionLinks>,
    }

    impl Backend for Fake {
        type Session = u64;
        fn start(&mut self, req: &StartRequest, _c: &Arc<Controller>, links: &SessionLinks) -> Result<u64> {
            if self.fail_next > 0 {
                self.fail_next -= 1;
                self.log.lock().unwrap().push("start-fail".into());
                return Err(anyhow!("device busy"));
            }
            self.log.lock().unwrap().push(format!("start:{}:{}", req.input, req.monitor_enabled));
            self.last_links = Some(links.clone());
            let _ = links.events.send(Event::Started {
                sample_rate: 48_000,
                mode: "shared",
                input_period_ms: 10.0,
                output_period_ms: 10.0,
                latency_ms: 30.0,
            });
            Ok(links.session_id)
        }
        fn set_monitor(&mut self, _s: &mut u64, enabled: bool, _d: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("monitor:{enabled}"));
            Ok(())
        }
        fn stop(&mut self, s: u64) {
            self.log.lock().unwrap().push(format!("stop:{s}"));
            if let Some(l) = &self.last_links {
                let _ = l.events.send(Event::Stopped);
            }
        }
        fn abandon(&mut self, s: u64) {
            self.log.lock().unwrap().push(format!("abandon:{s}"));
        }
        fn devices_event(&mut self) -> Result<Event> {
            Err(anyhow!("unused"))
        }
    }

    fn setup(fail_next: u32) -> (Supervisor<Fake>, Receiver<Event>, Arc<Mutex<Vec<String>>>) {
        let (etx, erx) = channel();
        let (ctx, _crx) = channel();
        let fake = Fake { fail_next, ..Fake::default() };
        let log = fake.log.clone();
        (Supervisor::new(fake, Arc::new(Controller::default()), etx, ctx), erx, log)
    }

    fn start_cmd() -> Command {
        Command::Start(StartParams { input: "Mic".into(), ..StartParams::default() })
    }

    fn kinds(rx: &Receiver<Event>) -> Vec<&'static str> {
        rx.try_iter()
            .map(|e| match e {
                Event::Started { .. } => "started",
                Event::Stopped => "stopped",
                Event::Reconnecting { .. } => "reconnecting",
                Event::Error { .. } => "error",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(1000));
        assert_eq!(backoff(4), Duration::from_secs(2));
        assert_eq!(backoff(40), Duration::from_secs(2));
    }

    #[test]
    fn device_loss_reconnects_without_stopped() {
        let (mut sup, rx, log) = setup(0);
        sup.handle(start_cmd());
        sup.handle(Command::Monitor { enabled: true, device: "Phones".into() });
        sup.device_lost(1, "input device lost".into(), Instant::now() + STABLE_AFTER);
        assert!(sup.is_reconnecting() && !sup.is_running());
        sup.tick(Instant::now() + RETRY_MAX);
        assert!(sup.is_running() && !sup.is_reconnecting());
        assert_eq!(kinds(&rx), ["started", "reconnecting", "started"]);
        // The monitor that was enabled after start is restored on reconnect.
        assert_eq!(*log.lock().unwrap(), ["start:Mic:false", "monitor:true", "abandon:1", "start:Mic:true", "monitor:true"]);
    }

    #[test]
    fn failed_attempts_back_off_until_success() {
        let (mut sup, rx, _) = setup(0);
        sup.handle(start_cmd());
        sup.backend.fail_next = 2;
        sup.device_lost(1, "output device lost".into(), Instant::now() + STABLE_AFTER);
        let mut t = Instant::now();
        for _ in 0..3 {
            t += RETRY_MAX;
            sup.tick(t);
        }
        assert!(sup.is_running());
        let events: Vec<Event> = rx.try_iter().collect();
        let attempts: Vec<u32> =
            events.iter().filter_map(|e| if let Event::Reconnecting { attempt, .. } = e { Some(*attempt) } else { None }).collect();
        assert_eq!(attempts, [1, 2, 3]);
        let reasons: Vec<&str> =
            events.iter().filter_map(|e| if let Event::Reconnecting { reason, .. } = e { Some(reason.as_str()) } else { None }).collect();
        assert_eq!(reasons[2], "output device lost: device busy");
    }

    #[test]
    fn stop_during_reconnect_cancels_and_reports_stopped() {
        let (mut sup, rx, _) = setup(0);
        sup.handle(start_cmd());
        sup.backend.fail_next = 100;
        sup.device_lost(1, "input device lost".into(), Instant::now() + STABLE_AFTER);
        sup.handle(Command::Stop);
        assert!(!sup.is_reconnecting() && sup.deadline().is_none());
        sup.tick(Instant::now() + RETRY_MAX);
        assert!(!sup.is_running());
        assert_eq!(kinds(&rx), ["started", "reconnecting", "stopped"]);
    }

    #[test]
    fn stale_device_loss_is_ignored() {
        let (mut sup, rx, log) = setup(0);
        sup.handle(start_cmd());
        sup.handle(start_cmd()); // session 2 replaces session 1
        sup.device_lost(1, "input device lost".into(), Instant::now() + STABLE_AFTER);
        assert!(sup.is_running() && !sup.is_reconnecting());
        assert!(!log.lock().unwrap().iter().any(|l| l.starts_with("abandon")));
        assert!(!kinds(&rx).contains(&"reconnecting"));
    }

    #[test]
    fn failed_start_does_not_reconnect() {
        let (mut sup, rx, _) = setup(1);
        sup.handle(start_cmd());
        assert!(!sup.is_running() && !sup.is_reconnecting());
        assert_eq!(kinds(&rx), ["error"]);
    }

    fn reconnect_attempts(rx: &Receiver<Event>) -> Vec<u32> {
        rx.try_iter().filter_map(|e| if let Event::Reconnecting { attempt, .. } = e { Some(attempt) } else { None }).collect()
    }

    #[test]
    fn a_device_that_fails_right_after_reopening_keeps_backing_off() {
        let (mut sup, rx, _) = setup(0);
        sup.handle(start_cmd());
        let mut t = Instant::now() + STABLE_AFTER;
        sup.device_lost(1, "input device lost".into(), t);
        for expected in [2u32, 3, 4] {
            t += RETRY_MAX;
            sup.tick(t);
            sup.device_lost(sup.session_id, "input device lost".into(), t + Duration::from_millis(10));
            assert_eq!(reconnect_attempts(&rx).last(), Some(&expected));
        }
        // After a stable run, the next loss starts over.
        t += RETRY_MAX;
        sup.tick(t);
        sup.device_lost(sup.session_id, "input device lost".into(), t + STABLE_AFTER);
        assert_eq!(reconnect_attempts(&rx).last(), Some(&1));
    }
}

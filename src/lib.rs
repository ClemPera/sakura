mod error;
mod state;
pub mod discovery;

pub use error::YeelightError;
pub use state::{ColorMode, LightState};

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
// use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

pub type Result<T> = std::result::Result<T, YeelightError>;

// Properties requested at connect time to seed the initial state.
const INIT_PROPS: &[&str] = &[
    "power", "bright", "ct", "rgb", "hue", "sat",
    "color_mode", "flowing", "music_on", "name",
];

// ------------------------------------------------------------------ public types

/// Transition effect for commands that support it.
///
/// `Smooth(duration_ms)` — `duration_ms` is clamped to a minimum of 30 ms
/// as required by the protocol.
#[derive(Debug, Clone, Copy)]
pub enum Transition {
    Sudden,
    Smooth(u32),
}

impl Transition {
    fn effect(self) -> &'static str {
        match self {
            Self::Sudden    => "sudden",
            Self::Smooth(_) => "smooth",
        }
    }
    fn duration(self) -> u32 {
        match self {
            Self::Sudden    => 0,
            Self::Smooth(d) => d.max(30),
        }
    }
}

/// Direction of a relative adjustment (`set_adjust`).
#[derive(Debug, Clone, Copy)]
pub enum AdjustAction {
    Increase,
    Decrease,
    /// Cycle: after reaching the max value, wrap back to the minimum.
    Circle,
}

impl AdjustAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Increase => "increase",
            Self::Decrease => "decrease",
            Self::Circle   => "circle",
        }
    }
}

/// Property target for `set_adjust`.
///
/// Note: `Color` only accepts [`AdjustAction::Circle`].
#[derive(Debug, Clone, Copy)]
pub enum AdjustProp {
    Bright,
    Ct,
    Color,
}

impl AdjustProp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bright => "bright",
            Self::Ct     => "ct",
            Self::Color  => "color",
        }
    }
}

/// Optional mode to switch into when powering on via `set_power`.
#[derive(Debug, Clone, Copy)]
pub enum PowerMode {
    Normal    = 0,
    Ct        = 1,
    Rgb       = 2,
    Hsv       = 3,
    ColorFlow = 4,
    /// Ceiling light only.
    Night     = 5,
}

// ------------------------------------------------------------------ client

/// A persistent TCP client for a single Yeelight bulb.
///
/// ## Connection
/// Call [`YeelightClient::connect`] with a known `ip:port` address, or
/// use [`discovery::discover`] first to find the bulb on the LAN and then
/// call `connect` with `device.addr`.
///
/// ## State tracking
/// A background thread reads [`NOTIFICATION`] messages pushed by the bulb
/// whenever its state changes (including changes triggered by other apps or
/// the physical button).  All outgoing commands also apply optimistic updates
/// so the state is accurate even before the next notification arrives.
///
/// Retrieve the current snapshot with [`YeelightClient::state`].
///
/// ## Thread safety
/// `YeelightClient` is `Send + Sync`; wrap it in an `Arc` to share across
/// FreeRTOS tasks on the ESP32.
pub struct YeelightClient {
    addr: SocketAddr,
    /// Write half — locked per command.
    stream: Arc<Mutex<TcpStream>>,
    state: Arc<Mutex<LightState>>,
    cmd_id: Arc<AtomicU32>,
    /// Stop flag for the current notification reader thread.
    /// Stored in a `Mutex` so it can be swapped on reconnect.
    reader_stop: Mutex<Arc<AtomicBool>>,
}

impl YeelightClient {
    /// Connect to a bulb at a known address (e.g. `"192.168.1.171:55443"`).
    ///
    /// Fetches the current state synchronously before returning so
    /// [`state()`](Self::state) is immediately populated.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or(YeelightError::InvalidParam("could not resolve address"))?;

        let mut stream = TcpStream::connect(addr)?;

        // Fetch state synchronously before handing the stream to the reader thread.
        let initial_state = Self::fetch_state_sync(&mut stream)?;

        let state = Arc::new(Mutex::new(initial_state));
        // id=1 was already used by the get_prop in fetch_state_sync.
        let cmd_id = Arc::new(AtomicU32::new(2));

        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Mutex::new(Arc::clone(&stop));

        // TODO
        // let read_stream = stream.try_clone()?;
        // let state_clone = Arc::clone(&state);
        // thread::spawn(move || Self::notification_reader(read_stream, state_clone, stop));

        Ok(Self {
            addr,
            stream: Arc::new(Mutex::new(stream)),
            state,
            cmd_id,
            reader_stop,
        })
    }

    /// Snapshot of the last known bulb state.
    pub fn state(&self) -> LightState {
        self.state.lock().unwrap().clone()
    }

    // ---------------------------------------------------------- private helpers

    /// Synchronous get_prop at connect time.
    /// Reads byte-by-byte to avoid `BufReader` consuming data the
    /// notification thread will need later.
    fn fetch_state_sync(stream: &mut TcpStream) -> Result<LightState> {
        let cmd = serde_json::to_string(&json!({
            "id": 1,
            "method": "get_prop",
            "params": INIT_PROPS,
        }))? + "\r\n";

        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(cmd.as_bytes())?;

        let line = Self::read_line_raw(stream)?;
        stream.set_read_timeout(None)?;

        // Parse {"id":1,"result":["on","100",...]}
        let v: Value = serde_json::from_str(&line)
            .map_err(|_| YeelightError::Protocol(format!("bad get_prop response: {line}")))?;

        let results = match v["result"].as_array() {
            Some(arr) => arr,
            // Bulb may reply with an error if get_prop isn't supported yet — fall back.
            None => return Ok(LightState::default()),
        };

        // Build a prop map in the same format apply_props expects (string values).
        let props: HashMap<String, Value> = INIT_PROPS
            .iter()
            .zip(results.iter())
            .map(|(k, v)| {
                // Ensure the value is always stored as a JSON string.
                let as_str = match v {
                    Value::String(s) => Value::String(s.clone()),
                    other            => Value::String(other.to_string()),
                };
                (k.to_string(), as_str)
            })
            .collect();

        let mut state = LightState::default();
        state.apply_props(&props);
        Ok(state)
    }

    /// Read a single `\r\n`-terminated line without using `BufReader`,
    /// to avoid accidentally consuming bytes meant for the reader thread.
    fn read_line_raw(stream: &TcpStream) -> std::io::Result<String> {
        let mut line = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        // &TcpStream implements Read, so we need a mutable binding.
        let mut s = stream;
        loop {
            match s.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    if byte[0] != b'\r' {
                        line.push(byte[0]);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    /// Background thread: reads NOTIFICATION messages and updates state.
    /// Exits cleanly when `stop` is set or the socket is closed.
    fn notification_reader(
        stream: TcpStream,
        state: Arc<Mutex<LightState>>,
        stop: Arc<AtomicBool>,
    ) {
        // Short read timeout so we can check the stop flag periodically.
        stream.set_read_timeout(Some(Duration::from_millis(500))).ok();

        let mut line_buf = Vec::with_capacity(512);
        let mut byte = [0u8; 1];
        let mut s = &stream;

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            match s.read(&mut byte) {
                Ok(0) => break, // EOF / connection closed
                Ok(_) => {
                    if byte[0] == b'\n' {
                        // We have a complete line.
                        let line = String::from_utf8_lossy(&line_buf).into_owned();
                        line_buf.clear();
                        Self::handle_notification(&line, &state);
                    } else if byte[0] != b'\r' {
                        line_buf.push(byte[0]);
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // Normal timeout — loop back and check stop flag.
                }
                Err(_) => break, // Real IO error — connection lost.
            }
        }
    }

    fn handle_notification(line: &str, state: &Mutex<LightState>) {
        if line.is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };

        // Only handle {"method":"props","params":{...}} notifications.
        if v["method"].as_str() != Some("props") {
            return;
        }
        let Some(params) = v["params"].as_object() else { return };

        let props: HashMap<String, Value> =
            params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        if let Ok(mut s) = state.lock() {
            s.apply_props(&props);
        }
    }

    fn next_id(&self) -> u32 {
        self.cmd_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, method: &str, params: Value) -> Result<()> {
        let id = self.next_id();
        let mut cmd = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;
        cmd.push_str("\r\n");

        let mut stream = self.stream.lock().unwrap();
        stream.write_all(cmd.as_bytes())?;
        Ok(())
    }

    /// Sends the command; on IO error attempts a single reconnect then retries.
    fn send_or_reconnect(&self, method: &str, params: Value) -> Result<()> {
        match self.send(method, params.clone()) {
            Err(YeelightError::Io(_)) => {
                self.reconnect()?;
                self.send(method, params)
            }
            other => other,
        }
    }

    fn reconnect(&self) -> Result<()> {
        // Signal the old reader thread to stop.
        self.reader_stop.lock().unwrap().store(true, Ordering::Relaxed);

        let new_stream = TcpStream::connect(self.addr)?;
        let read_stream = new_stream.try_clone()?;

        // Fresh stop flag for the new thread.
        let stop = Arc::new(AtomicBool::new(false));
        *self.reader_stop.lock().unwrap() = Arc::clone(&stop);

        let state_clone = Arc::clone(&self.state);
        thread::spawn(move || Self::notification_reader(read_stream, state_clone, stop));

        *self.stream.lock().unwrap() = new_stream;
        Ok(())
    }

    // ---------------------------------------------------------------- commands

    /// Toggle the bulb on/off without needing to know the current state.
    pub fn toggle(&self) -> Result<()> {
        let r = self.send_or_reconnect("toggle", json!([]));
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.power = Some(!s.power.unwrap_or(false));
            }
        }
        r
    }

    /// Toggle main light and background light simultaneously.
    pub fn dev_toggle(&self) -> Result<()> {
        self.send_or_reconnect("dev_toggle", json!([]))
    }

    /// Turn the bulb on or off.
    ///
    /// `mode` optionally switches the bulb into a specific color mode on power-on.
    pub fn set_power(
        &self,
        on: bool,
        transition: Transition,
        mode: Option<PowerMode>,
    ) -> Result<()> {
        let power_str = if on { "on" } else { "off" };
        let mut params = json!([power_str, transition.effect(), transition.duration()]);
        if let Some(m) = mode {
            params.as_array_mut().unwrap().push(json!(m as u8));
        }
        let r = self.send_or_reconnect("set_power", params);
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.power = Some(on);
            }
        }
        r
    }

    /// Set brightness (1–100 %).
    pub fn set_brightness(&self, brightness: u8, transition: Transition) -> Result<()> {
        if !(1..=100).contains(&brightness) {
            return Err(YeelightError::InvalidParam("brightness must be 1–100"));
        }
        let params = json!([brightness, transition.effect(), transition.duration()]);
        let r = self.send_or_reconnect("set_bright", params);
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.brightness = Some(brightness);
            }
        }
        r
    }

    /// Set color temperature in Kelvin (1700–6500 K).
    ///
    /// Only accepted while the bulb is on.
    pub fn set_ct(&self, ct: u16, transition: Transition) -> Result<()> {
        if !(1700..=6500).contains(&ct) {
            return Err(YeelightError::InvalidParam("ct must be 1700–6500 K"));
        }
        let params = json!([ct, transition.effect(), transition.duration()]);
        let r = self.send_or_reconnect("set_ct_abx", params);
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.ct = Some(ct);
                s.color_mode = Some(ColorMode::ColorTemp);
            }
        }
        r
    }

    /// Set RGB color (0–16_777_215 / `0x000000`–`0xFFFFFF`).
    ///
    /// Only accepted while the bulb is on.
    pub fn set_rgb(&self, rgb: u32, transition: Transition) -> Result<()> {
        if rgb > 0xFF_FFFF {
            return Err(YeelightError::InvalidParam("rgb must be 0–16_777_215"));
        }
        let params = json!([rgb, transition.effect(), transition.duration()]);
        let r = self.send_or_reconnect("set_rgb", params);
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.rgb = Some(rgb);
                s.color_mode = Some(ColorMode::Rgb);
            }
        }
        r
    }

    /// Set HSV color. `hue`: 0–359, `sat`: 0–100.
    ///
    /// Only accepted while the bulb is on.
    pub fn set_hsv(&self, hue: u16, sat: u8, transition: Transition) -> Result<()> {
        if hue > 359 {
            return Err(YeelightError::InvalidParam("hue must be 0–359"));
        }
        if sat > 100 {
            return Err(YeelightError::InvalidParam("sat must be 0–100"));
        }
        let params = json!([hue, sat, transition.effect(), transition.duration()]);
        let r = self.send_or_reconnect("set_hsv", params);
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.hue = Some(hue);
                s.sat = Some(sat);
                s.color_mode = Some(ColorMode::Hsv);
            }
        }
        r
    }

    /// Persist the current state as the power-on default.
    ///
    /// Only accepted while the bulb is on.
    pub fn set_default(&self) -> Result<()> {
        self.send_or_reconnect("set_default", json!([]))
    }

    /// Stop a running color flow.
    pub fn stop_cf(&self) -> Result<()> {
        self.send_or_reconnect("stop_cf", json!([]))
    }

    // -------------------------------------------------------- cron / sleep timer

    /// Start a sleep timer: power off after `minutes` (1–60).
    ///
    /// Only accepted while the bulb is on.
    pub fn cron_add(&self, minutes: u32) -> Result<()> {
        if minutes == 0 || minutes > 60 {
            return Err(YeelightError::InvalidParam("minutes must be 1–60"));
        }
        // type 0 = power-off (only type currently defined by the spec).
        self.send_or_reconnect("cron_add", json!([0, minutes]))
    }

    /// Cancel the active sleep timer.
    pub fn cron_del(&self) -> Result<()> {
        self.send_or_reconnect("cron_del", json!([0]))
    }

    // -------------------------------------------------------- relative adjustments

    /// Adjust a property by a relative step without knowing the current value.
    ///
    /// `Color` only accepts [`AdjustAction::Circle`].
    pub fn set_adjust(&self, action: AdjustAction, prop: AdjustProp) -> Result<()> {
        if matches!(prop, AdjustProp::Color) && !matches!(action, AdjustAction::Circle) {
            return Err(YeelightError::InvalidParam(
                "AdjustProp::Color only supports AdjustAction::Circle",
            ));
        }
        self.send_or_reconnect("set_adjust", json!([action.as_str(), prop.as_str()]))
    }

    /// Adjust brightness by `percentage` (-100–100) over `duration_ms`.
    pub fn adjust_bright(&self, percentage: i8, duration_ms: u32) -> Result<()> {
        if !(-100..=100).contains(&percentage) {
            return Err(YeelightError::InvalidParam("percentage must be -100–100"));
        }
        self.send_or_reconnect("adjust_bright", json!([percentage, duration_ms]))
    }

    /// Adjust color temperature by `percentage` (-100–100) over `duration_ms`.
    pub fn adjust_ct(&self, percentage: i8, duration_ms: u32) -> Result<()> {
        if !(-100..=100).contains(&percentage) {
            return Err(YeelightError::InvalidParam("percentage must be -100–100"));
        }
        self.send_or_reconnect("adjust_ct", json!([percentage, duration_ms]))
    }

    /// Cycle the color over `duration_ms`.
    ///
    /// Note: the percentage step is defined internally by the bulb and cannot
    /// be set; only the duration is configurable.
    pub fn adjust_color(&self, duration_ms: u32) -> Result<()> {
        self.send_or_reconnect("adjust_color", json!([0, duration_ms]))
    }

    // -------------------------------------------------------- scenes

    /// Directly set a solid RGB color + brightness (works even when off).
    pub fn scene_color(&self, rgb: u32, brightness: u8) -> Result<()> {
        if rgb > 0xFF_FFFF {
            return Err(YeelightError::InvalidParam("rgb must be 0–16_777_215"));
        }
        if !(1..=100).contains(&brightness) {
            return Err(YeelightError::InvalidParam("brightness must be 1–100"));
        }
        self.send_or_reconnect("set_scene", json!(["color", rgb, brightness]))
    }

    /// Directly set an HSV color + brightness (works even when off).
    pub fn scene_hsv(&self, hue: u16, sat: u8, brightness: u8) -> Result<()> {
        if hue > 359 {
            return Err(YeelightError::InvalidParam("hue must be 0–359"));
        }
        if sat > 100 {
            return Err(YeelightError::InvalidParam("sat must be 0–100"));
        }
        if !(1..=100).contains(&brightness) {
            return Err(YeelightError::InvalidParam("brightness must be 1–100"));
        }
        self.send_or_reconnect("set_scene", json!(["hsv", hue, sat, brightness]))
    }

    /// Directly set a color temperature + brightness (works even when off).
    pub fn scene_ct(&self, ct: u16, brightness: u8) -> Result<()> {
        if !(1700..=6500).contains(&ct) {
            return Err(YeelightError::InvalidParam("ct must be 1700–6500 K"));
        }
        if !(1..=100).contains(&brightness) {
            return Err(YeelightError::InvalidParam("brightness must be 1–100"));
        }
        self.send_or_reconnect("set_scene", json!(["ct", ct, brightness]))
    }

    /// Turn on at `brightness`% then auto-off after `minutes` (works even when off).
    pub fn scene_auto_delay_off(&self, brightness: u8, minutes: u32) -> Result<()> {
        if !(1..=100).contains(&brightness) {
            return Err(YeelightError::InvalidParam("brightness must be 1–100"));
        }
        self.send_or_reconnect(
            "set_scene",
            json!(["auto_delay_off", brightness, minutes]),
        )
    }

    // -------------------------------------------------------- misc

    /// Start or stop music mode.
    ///
    /// In music mode the bulb connects back to **your** TCP server at
    /// (`host`, `port`), rate limits are lifted, and no properties are reported.
    /// You must open the TCP server before calling this.
    pub fn set_music(
        &self,
        enable: bool,
        host: Option<&str>,
        port: Option<u16>,
    ) -> Result<()> {
        let params = if enable {
            let h = host.ok_or(YeelightError::InvalidParam(
                "host required when enabling music mode",
            ))?;
            let p = port.ok_or(YeelightError::InvalidParam(
                "port required when enabling music mode",
            ))?;
            json!([1, h, p])
        } else {
            json!([0])
        };
        self.send_or_reconnect("set_music", params)
    }

    /// Set the device name (stored in persistent memory, max 64 bytes).
    pub fn set_name(&self, name: &str) -> Result<()> {
        if name.len() > 64 {
            return Err(YeelightError::InvalidParam("name must be ≤ 64 bytes"));
        }
        let r = self.send_or_reconnect("set_name", json!([name]));
        if r.is_ok() {
            if let Ok(mut s) = self.state.lock() {
                s.name = Some(name.to_string());
            }
        }
        r
    }
}

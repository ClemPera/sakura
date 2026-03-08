use std::collections::HashMap;
use serde_json::Value;

/// Current active color mode of the bulb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Rgb      = 1,
    ColorTemp = 2,
    Hsv      = 3,
}

impl TryFrom<u8> for ColorMode {
    type Error = ();
    fn try_from(v: u8) -> Result<Self, ()> {
        match v {
            1 => Ok(Self::Rgb),
            2 => Ok(Self::ColorTemp),
            3 => Ok(Self::Hsv),
            _ => Err(()),
        }
    }
}

/// Last known state of the bulb.
///
/// Fields are `Option` because the bulb may not report all of them
/// depending on the model / current mode.  Updated both optimistically
/// (on every outgoing command) and reactively (via NOTIFICATION messages
/// from the bulb, including changes made by other apps or physical buttons).
#[derive(Debug, Clone, Default)]
pub struct LightState {
    /// `true` = on, `false` = off.
    pub power: Option<bool>,
    /// Brightness percentage 1–100.
    pub brightness: Option<u8>,
    /// Color temperature in Kelvin (1700–6500). Valid when `color_mode == ColorTemp`.
    pub ct: Option<u16>,
    /// RGB value 0–16_777_215. Valid when `color_mode == Rgb`.
    pub rgb: Option<u32>,
    /// Hue 0–359. Valid when `color_mode == Hsv`.
    pub hue: Option<u16>,
    /// Saturation 0–100. Valid when `color_mode == Hsv`.
    pub sat: Option<u8>,
    pub color_mode: Option<ColorMode>,
    /// A color flow is currently running.
    pub flowing: Option<bool>,
    /// Music mode is active (no rate limiting, no property reports).
    pub music_on: Option<bool>,
    /// Name stored on the device.
    pub name: Option<String>,
}

impl LightState {
    /// Merge a set of key/value property pairs into this state.
    /// This is called both for `get_prop` results and `props` notifications.
    /// All values are expected to be JSON strings (as the protocol sends them).
    pub(crate) fn apply_props(&mut self, props: &HashMap<String, Value>) {
        for (key, value) in props {
            let s = value.as_str().unwrap_or_default();
            match key.as_str() {
                "power"      => self.power      = Some(s == "on"),
                "bright"     => self.brightness = s.parse().ok(),
                "ct"         => self.ct         = s.parse().ok(),
                "rgb"        => self.rgb        = s.parse().ok(),
                "hue"        => self.hue        = s.parse().ok(),
                "sat"        => self.sat        = s.parse().ok(),
                "color_mode" => {
                    self.color_mode = s.parse::<u8>().ok()
                        .and_then(|n| ColorMode::try_from(n).ok());
                }
                "flowing"  => self.flowing  = Some(s == "1"),
                "music_on" => self.music_on = Some(s == "1"),
                "name"     => self.name     = Some(s.to_string()),
                _          => {}
            }
        }
    }
}

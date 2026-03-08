# 🌸 sakura

> A Rust library for controlling Yeelight smart bulbs over local TCP, designed to run on ESP32-S3 with [`esp-idf-svc`](https://github.com/esp-rs/esp-idf-svc) but can also run on any other devices.

---

## Features

- **Full command coverage** — toggle, power, brightness, RGB, HSV, color temperature, scenes, cron timers, relative adjustments, music mode, and more
- **State tracking** — last known bulb state is kept in sync both optimistically (on every outgoing command).
- **Auto-reconnect** — transparent reconnection on TCP drop; the background reader thread is respawned automatically
- **LAN discovery** — optional SSDP-based discovery to find bulbs without a known IP
- **Typed API** — validated parameters with descriptive errors before anything hits the wire
- **ESP32-ready** — works with `std` on `xtensa-esp32s3-espidf`; no async runtime required

---

## Usage

### Connect with a known IP

```rust
use sakura::{YeelightClient, Transition, PowerMode};

let bulb = YeelightClient::connect("192.168.1.171:55443")?;

bulb.toggle()?;
bulb.set_power(true, Transition::Smooth(500), Some(PowerMode::Rgb))?;
bulb.set_rgb(0xFF2200, Transition::Smooth(300))?;
bulb.set_brightness(40, Transition::Sudden)?;
```

### Discover on the LAN first

```rust
use std::time::Duration;
use sakura::discovery;

let devices = discovery::discover(Duration::from_secs(2))?;
let bulb = YeelightClient::connect(devices[0].addr)?;
```

### Read current state

```rust
// Updated by your own commands AND by external changes (app, physical button, etc.)
let s = bulb.state();
println!("on={:?}  bright={:?}  mode={:?}", s.power, s.brightness, s.color_mode);
```

---

## API overview

| Method | Description |
|---|---|
| `toggle()` | Flip on/off without knowing current state |
| `set_power(on, transition, mode)` | Turn on/off, optionally switching color mode |
| `set_brightness(pct, transition)` | 1–100 % |
| `set_rgb(value, transition)` | 0x000000–0xFFFFFF |
| `set_hsv(hue, sat, transition)` | hue 0–359, sat 0–100 |
| `set_ct(kelvin, transition)` | 1700–6500 K |
| `set_default()` | Persist current state as power-on default |
| `set_adjust(action, prop)` | Relative adjustment (increase/decrease/circle) |
| `adjust_bright(pct, ms)` | Relative brightness step |
| `adjust_ct(pct, ms)` | Relative color temperature step |
| `adjust_color(ms)` | Cycle color |
| `scene_color(rgb, bright)` | Jump directly to RGB scene (works when off) |
| `scene_hsv(h, s, bright)` | Jump directly to HSV scene (works when off) |
| `scene_ct(kelvin, bright)` | Jump directly to CT scene (works when off) |
| `scene_auto_delay_off(bright, min)` | Turn on then auto-off after N minutes |
| `cron_add(minutes)` | Sleep timer (power off after N minutes) |
| `cron_del()` | Cancel active sleep timer |
| `set_music(enable, host, port)` | Enable/disable music mode |
| `set_name(name)` | Set device name (persisted on device) |
| `stop_cf()` | Stop a running color flow |
| `dev_toggle()` | Toggle main + background light simultaneously |
| `state()` | Snapshot of the last known `LightState` |

`Transition` is either `Transition::Sudden` or `Transition::Smooth(duration_ms)`.

---

## Protocol

Implements the [Yeelight WiFi Light Inter-Operation Specification](https://www.yeelight.com/download/Yeelight_Inter-Operation_Spec.pdf) — JSON over TCP on port `55443`, messages terminated by `\r\n`.

---

## Notes

- The bulb supports up to **4 simultaneous TCP connections** and a quota of **60 commands/minute** per connection (144/min total across all LAN connections). Music mode lifts this limit.

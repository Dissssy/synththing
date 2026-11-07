//! Maths helpers for scripts that nearly every visual one ends up writing
//! itself: smooth noise (`noise`) and easing curves (`ease`).

use std::f64::consts::PI;

use mlua::Lua;
use noise::{NoiseFn, Simplex};

/// The easing curves `ease` knows, by name: `in_` speeds up from rest,
/// `out_` slows into rest, `in_out_` does both.
const CURVES: &[&str] = &["sine", "quad", "cubic", "quart", "quint", "expo", "circ", "back", "elastic", "bounce"];

/// A curve going in (speeding up from 0 to 1).
fn ease_in(curve: &str, t: f64) -> f64 {
    match curve {
        "sine" => 1.0 - (t * PI / 2.0).cos(),
        "quad" => t.powi(2),
        "cubic" => t.powi(3),
        "quart" => t.powi(4),
        "quint" => t.powi(5),
        "expo" if t <= 0.0 => 0.0,
        "expo" => 2f64.powf(10.0 * t - 10.0),
        "circ" => 1.0 - (1.0 - t * t).sqrt(),
        "back" => 2.70158 * t.powi(3) - 1.70158 * t * t,
        "elastic" if t <= 0.0 || t >= 1.0 => t,
        "elastic" => -(2f64.powf(10.0 * t - 10.0)) * ((t * 10.0 - 10.75) * (2.0 * PI / 3.0)).sin(),
        "bounce" => 1.0 - bounce_out(1.0 - t),
        _ => unreachable!("checked against CURVES"),
    }
}

fn bounce_out(t: f64) -> f64 {
    const N: f64 = 7.5625;
    const D: f64 = 2.75;
    if t < 1.0 / D {
        N * t * t
    } else if t < 2.0 / D {
        let t = t - 1.5 / D;
        N * t * t + 0.75
    } else if t < 2.5 / D {
        let t = t - 2.25 / D;
        N * t * t + 0.9375
    } else {
        let t = t - 2.625 / D;
        N * t * t + 0.984375
    }
}

/// `t` (0 to 1, clamped) along the named curve.
pub fn ease(name: &str, t: f64) -> Result<f64, String> {
    let t = t.clamp(0.0, 1.0);
    if name == "linear" {
        return Ok(t);
    }
    let (way, curve) = name
        .strip_prefix("in_out_")
        .map(|c| ("in_out", c))
        .or_else(|| name.strip_prefix("in_").map(|c| ("in", c)))
        .or_else(|| name.strip_prefix("out_").map(|c| ("out", c)))
        .filter(|(_, c)| CURVES.contains(c))
        .ok_or_else(|| {
            format!("ease: unknown curve {name:?} (linear, or in_, out_ or in_out_ then one of {})", CURVES.join(", "))
        })?;
    Ok(match way {
        "in" => ease_in(curve, t),
        "out" => 1.0 - ease_in(curve, 1.0 - t),
        _ if t < 0.5 => ease_in(curve, 2.0 * t) / 2.0,
        _ => 1.0 - ease_in(curve, 2.0 - 2.0 * t) / 2.0,
    })
}

pub fn register(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    // One fixed seed: the same everywhere, recordings included. Shift the
    // inputs for a different-looking field.
    let simplex = Simplex::new(0);
    globals.set(
        "noise",
        lua.create_function(move |_, (x, y, z): (f64, Option<f64>, Option<f64>)| {
            Ok(match z {
                Some(z) => simplex.get([x, y.unwrap_or(0.0), z]),
                None => simplex.get([x, y.unwrap_or(0.0)]),
            }
            .clamp(-1.0, 1.0))
        })?,
    )?;

    globals.set("ease", lua.create_function(|_, (name, t): (String, f64)| ease(&name, t).map_err(mlua::Error::runtime))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_runs_from_0_to_1() {
        for curve in CURVES {
            for way in ["in_", "out_", "in_out_"] {
                let name = format!("{way}{curve}");
                assert!(ease(&name, 0.0).unwrap().abs() < 1e-9, "{name} at 0");
                assert!((ease(&name, 1.0).unwrap() - 1.0).abs() < 1e-9, "{name} at 1");
                if way == "in_out_" {
                    assert!((ease(&name, 0.5).unwrap() - 0.5).abs() < 1e-9, "{name} at 0.5");
                }
            }
        }
        assert_eq!(ease("linear", 2.0), Ok(1.0));
        assert!(ease("in_wobble", 0.5).is_err());
        assert!(ease("cubic", 0.5).is_err());
    }

    #[test]
    fn noise_is_smooth_and_in_range() {
        let lua = Lua::new();
        register(&lua).unwrap();
        let (a, b, spread): (f64, f64, f64) = lua
            .load(
                r#"
                local lo, hi = 0, 0
                for i = 0, 2000 do
                    local v = noise(i * 0.137, i * 0.071, i * 0.013)
                    lo, hi = math.min(lo, v), math.max(hi, v)
                end
                return noise(3.2), noise(3.2001), hi - lo
                "#,
            )
            .eval()
            .unwrap();
        assert!((a - b).abs() < 0.01);
        assert!(spread > 1.0 && spread <= 2.0, "{spread}");
    }
}

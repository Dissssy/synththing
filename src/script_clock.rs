//! The date and time scripts see (`now`, `unix_time`, `format_time`).
//!
//! Wall time normally. In a recording (and so in previews) it's recording
//! time instead: the clock reads the moment the recording began and moves
//! on with the recorded frames, so a clock drawn into the video shows the
//! time it would have shown played live, however fast the video renders.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Datelike, FixedOffset, Local, Offset, TimeZone, Timelike, Utc};
use mlua::{Lua, Table, Value};

#[derive(Default)]
pub struct ScriptClock {
    /// While recording: the wall time it began at, and `TIME` then.
    anchor: Option<(f64, f64)>,
    /// `TIME` as of the last frame.
    time: f64,
}

impl ScriptClock {
    /// Each frame, before `render()`: the script's `TIME`, and whether a
    /// recording is under way.
    pub fn frame(&mut self, time: f64, recording: bool) {
        self.time = time;
        match (recording, self.anchor) {
            (true, None) => self.anchor = Some((wall_seconds(), time)),
            (false, Some(_)) => self.anchor = None,
            _ => {}
        }
    }

    /// Seconds since the Unix epoch, as the script sees them.
    pub fn unix(&self) -> f64 {
        match self.anchor {
            Some((wall, time)) => wall + (self.time - time),
            None => wall_seconds(),
        }
    }
}

fn wall_seconds() -> f64 {
    Utc::now().timestamp_micros() as f64 / 1e6
}

/// A Unix time (seconds, fractional) as a date in the local time zone, or
/// in UTC.
fn date_at(unix: f64, utc: bool) -> mlua::Result<DateTime<FixedOffset>> {
    let at = DateTime::from_timestamp_micros((unix * 1e6).round() as i64)
        .ok_or_else(|| mlua::Error::runtime(format!("time out of range: {unix}")))?;
    let offset = if utc { Utc.fix() } else { Local.offset_from_utc_datetime(&at.naive_utc()).fix() };
    Ok(at.with_timezone(&offset))
}

fn date_table(lua: &Lua, unix: f64, date: &DateTime<FixedOffset>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("year", date.year())?;
    t.raw_set("month", date.month())?;
    t.raw_set("day", date.day())?;
    t.raw_set("hour", date.hour())?;
    t.raw_set("minute", date.minute())?;
    t.raw_set("second", date.second())?;
    t.raw_set("ms", date.timestamp_subsec_millis().min(999))?;
    t.raw_set("weekday", date.weekday().number_from_monday())?;
    t.raw_set("yearday", date.ordinal())?;
    t.raw_set("utc_offset", date.offset().local_minus_utc())?;
    t.raw_set("unix", unix)?;
    Ok(t)
}

pub fn register(lua: &Lua, clock: &Rc<RefCell<ScriptClock>>) -> mlua::Result<()> {
    let globals = lua.globals();

    let c = Rc::clone(clock);
    globals.set(
        "now",
        lua.create_function(move |lua, utc: Option<bool>| {
            let unix = c.borrow().unix();
            date_table(lua, unix, &date_at(unix, utc.unwrap_or(false))?)
        })?,
    )?;

    let c = Rc::clone(clock);
    globals.set("unix_time", lua.create_function(move |_, ()| Ok(c.borrow().unix()))?)?;

    let c = Rc::clone(clock);
    globals.set(
        "format_time",
        lua.create_function(move |_, (format, at): (String, Value)| {
            let date = match at {
                Value::Nil => date_at(c.borrow().unix(), false)?,
                Value::Integer(unix) => date_at(unix as f64, false)?,
                Value::Number(unix) => date_at(unix, false)?,
                // A table from now(): its instant, in its own offset.
                Value::Table(t) => {
                    let unix: f64 = t.get("unix")?;
                    let offset: Option<i32> = t.get("utc_offset")?;
                    let date = date_at(unix, true)?;
                    let offset = FixedOffset::east_opt(offset.unwrap_or(0))
                        .ok_or_else(|| mlua::Error::runtime("format_time: utc_offset out of range"))?;
                    date.with_timezone(&offset)
                }
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "format_time: expected a time from now() or unix_time(), got {}",
                        other.type_name()
                    )));
                }
            };
            let items: Vec<Item> = StrftimeItems::new(&format)
                .parse()
                .map_err(|_| mlua::Error::runtime(format!("format_time: bad format {format:?}")))?;
            Ok(date.format_with_items(items.iter()).to_string())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_time_runs_with_frames() {
        let mut clock = ScriptClock::default();
        clock.frame(10.0, true);
        let start = clock.unix();
        clock.frame(70.0, true);
        assert!((clock.unix() - start - 60.0).abs() < 1e-6);
        clock.frame(71.0, false);
        assert!((clock.unix() - wall_seconds()).abs() < 1.0);
    }

    #[test]
    fn formats_and_splits_dates() {
        let lua = Lua::new();
        let clock = Rc::new(RefCell::new(ScriptClock::default()));
        register(&lua, &clock).unwrap();
        let (text, year, weekday, bad): (String, i32, u32, bool) = lua
            .load(
                r#"
                local t = now(true)
                local ok = pcall(format_time, "%Q")
                return format_time("%Y-%m-%d %H:%M", 0), t.year, t.weekday, ok
                "#,
            )
            .eval()
            .unwrap();
        assert_eq!(text.len(), 16);
        assert!(year >= 2026);
        assert!((1..=7).contains(&weekday));
        assert!(!bad);
        let utc: String = lua.load(r#"return format_time("%Y-%m-%d %H:%M:%S", { unix = 86400.5, utc_offset = 0 })"#).eval().unwrap();
        assert_eq!(utc, "1970-01-02 00:00:00");
    }
}

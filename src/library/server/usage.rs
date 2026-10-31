//! Anonymous usage data (docs/USAGE.md): the counts apps that opted in
//! send, added up by version, platform, level, target and signature. No
//! address, key or ID is kept with them, so a row says how often something
//! happened, never to whom. Kept until an admin clears it
//! (`synththing admin usage --clear`); `new` counts what came in since
//! intake last took it.
//!
//! Intake (docs/feedback-intake.md) collects what's new as one flat list,
//! a line's versions and platforms together (`GET /api/v1/intake/usage`),
//! and says which issue each line it filed became
//! (`POST /api/v1/intake/usage/taken`). The server remembers that, by the
//! line's signature without its line number (`key`), so the same line is
//! never filed twice, in a later run or a later version.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tiny_http::Request;

use super::moderation::limit;
use super::{error, json, now, read_body, Reply, State};
use crate::library::usage::{
    PerfGroup, PerfRow, SectionStat, FRAME_BUCKETS_US, MAX_PERF_GROUPS, MAX_PERF_SECTIONS,
    kept, parse_rules, UsageNode, UsageReport, UsageRule, DEFAULT_USAGE_RULES, MAX_USAGE_LINES, MAX_USAGE_TEXT, USAGE_LEVELS,
};

pub(super) const SCHEMA_V21: &str = "
BEGIN;
CREATE TABLE usage (
    version TEXT NOT NULL,
    platform TEXT NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    signature TEXT NOT NULL,
    total INTEGER NOT NULL,
    new INTEGER NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (version, platform, level, target, signature)
);
CREATE TABLE usage_sessions (
    version TEXT NOT NULL,
    platform TEXT NOT NULL,
    total INTEGER NOT NULL,
    new INTEGER NOT NULL,
    PRIMARY KEY (version, platform)
);
CREATE TABLE usage_rules (position INTEGER PRIMARY KEY, rule TEXT NOT NULL);
INSERT INTO usage_rules (rule) VALUES ('error * keep'), ('warn * keep'), ('info * drop');
PRAGMA user_version = 21;
COMMIT;
";

/// What intake was last offered (`offered`, taken away from `new` once it
/// says it's taken), and the issue each line became.
pub(super) const SCHEMA_V22: &str = "
BEGIN;
ALTER TABLE usage ADD COLUMN offered INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_sessions ADD COLUMN offered INTEGER NOT NULL DEFAULT 0;
CREATE TABLE usage_filed (
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    key TEXT NOT NULL,
    issue INTEGER NOT NULL,
    PRIMARY KEY (level, target, key)
);
PRAGMA user_version = 22;
COMMIT;
";

/// Frame times (docs/USAGE.md): per version, platform, hardware and
/// settings (each kept as its JSON), frames per bucket, and the sections'
/// time.
pub(super) const SCHEMA_V23: &str = "
BEGIN;
CREATE TABLE usage_perf (
    version TEXT NOT NULL,
    platform TEXT NOT NULL,
    hardware TEXT NOT NULL,
    settings TEXT NOT NULL,
    frames TEXT NOT NULL,
    PRIMARY KEY (version, platform, hardware, settings)
);
CREATE TABLE usage_perf_sections (
    version TEXT NOT NULL,
    platform TEXT NOT NULL,
    hardware TEXT NOT NULL,
    settings TEXT NOT NULL,
    name TEXT NOT NULL,
    total_us INTEGER NOT NULL,
    frames INTEGER NOT NULL,
    worst_us INTEGER NOT NULL,
    PRIMARY KEY (version, platform, hardware, settings, name)
);
PRAGMA user_version = 23;
COMMIT;
";

/// The most lines one collection hands intake; the rest wait for the next.
const DIGEST_LINES: usize = 100;
/// Examples given of each, at most: signatures, versions, platforms.
const DIGEST_EXAMPLES: usize = 5;

/// Reports one address may send in a day: the daily one, and a few for
/// panics, which go as soon as the app starts again.
const REPORTS_PER_DAY: usize = 6;

/// The tree's levels, in order: the `usage` columns.
const LEVELS: [&str; 5] = ["version", "platform", "level", "target", "signature"];

/// The rules, in order.
pub(super) fn rules(db: &Connection) -> rusqlite::Result<Vec<String>> {
    db.prepare("SELECT rule FROM usage_rules ORDER BY position")?.query_map([], |r| r.get(0))?.collect()
}

/// `GET /api/v1/usage/rules`: which lines the server wants, so the app
/// sends only those.
pub(super) fn get_rules(state: &State) -> Reply {
    match rules(&state.db()) {
        Ok(rules) => json(200, &serde_json::json!({ "rules": rules })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/usage`: a report, added to the counts.
pub(super) fn report(state: &State, request: &mut Request, ip: &str) -> Reply {
    if let Err(reply) = limit(state, "usage", ip, REPORTS_PER_DAY, Duration::from_secs(24 * 3600), "usage reports") {
        return reply;
    }
    let body = match read_body(request, MAX_USAGE_LINES * (3 * MAX_USAGE_TEXT + 64) + 4096) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let report: UsageReport = match serde_json::from_slice(&body) {
        Ok(report) => report,
        Err(e) => return error(400, format!("not a usage report: {e}")),
    };
    if let Err(e) = check(&report) {
        return error(400, e);
    }
    let mut db = state.db();
    match add(&mut db, &report) {
        Ok(lines) => json(200, &serde_json::json!({ "kept": lines })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

fn check(report: &UsageReport) -> Result<(), String> {
    let short = |s: &str| !s.trim().is_empty() && s.len() <= MAX_USAGE_TEXT;
    if !short(&report.version) || !short(&report.platform) {
        return Err("the version and platform are needed, and short".into());
    }
    if report.lines.len() > MAX_USAGE_LINES {
        return Err(format!("a report has up to {MAX_USAGE_LINES} lines"));
    }
    for line in &report.lines {
        if !USAGE_LEVELS.contains(&line.level.as_str()) {
            return Err(format!("\"{}\" isn't a level", line.level));
        }
        if !short(&line.target) || !short(&line.signature) {
            return Err("a line's target and signature are needed, and short".into());
        }
    }
    if report.perf.len() > MAX_PERF_GROUPS {
        return Err(format!("a report has up to {MAX_PERF_GROUPS} groups of frames"));
    }
    for group in &report.perf {
        if group.frames.len() != FRAME_BUCKETS_US.len() + 1 || group.sections.len() > MAX_PERF_SECTIONS {
            return Err("a group of frames has a count per bucket and a few sections".into());
        }
        let hardware = &group.hardware;
        if hardware.gpu.len() > MAX_USAGE_TEXT || hardware.backend.len() > MAX_USAGE_TEXT {
            return Err("the hardware's names are short".into());
        }
        if group.sections.iter().any(|s| !short(&s.name)) {
            return Err("a section's name is needed, and short".into());
        }
    }
    Ok(())
}

/// Add a report's frame times to the counts.
fn add_perf(tx: &rusqlite::Transaction, report: &UsageReport) -> rusqlite::Result<()> {
    for group in &report.perf {
        let hardware = serde_json::to_string(&group.hardware).unwrap_or_default();
        let settings = serde_json::to_string(&group.settings).unwrap_or_default();
        let key = params![report.version, report.platform, hardware, settings];
        let stored: Option<String> = tx
            .query_row(
                "SELECT frames FROM usage_perf WHERE version = ? AND platform = ? AND hardware = ? AND settings = ?",
                key,
                |r| r.get(0),
            )
            .optional()?;
        let mut frames: Vec<u64> = stored.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        frames.resize(FRAME_BUCKETS_US.len() + 1, 0);
        for (count, new) in frames.iter_mut().zip(&group.frames) {
            *count += new;
        }
        tx.execute(
            "INSERT INTO usage_perf (version, platform, hardware, settings, frames) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO UPDATE SET frames = ?5",
            params![report.version, report.platform, hardware, settings, serde_json::to_string(&frames).unwrap_or_default()],
        )?;
        for section in &group.sections {
            tx.execute(
                "INSERT INTO usage_perf_sections (version, platform, hardware, settings, name, total_us, frames, worst_us)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET total_us = total_us + ?6, frames = frames + ?7, worst_us = MAX(worst_us, ?8)",
                params![
                    report.version,
                    report.platform,
                    hardware,
                    settings,
                    section.name,
                    section.total_us as i64,
                    section.frames as i64,
                    section.worst_us as i64
                ],
            )?;
        }
    }
    Ok(())
}

/// Frame times, the most frames first: all of them, or one version's.
pub(super) fn perf(db: &Connection, version: Option<&str>) -> rusqlite::Result<Vec<PerfRow>> {
    let mut rows: Vec<(PerfRow, String, String)> = db
        .prepare("SELECT version, platform, hardware, settings, frames FROM usage_perf WHERE ?1 IS NULL OR version = ?1")?
        .query_map([version], |r| {
            let (hardware, settings, frames): (String, String, String) = (r.get(2)?, r.get(3)?, r.get(4)?);
            let group = PerfGroup {
                hardware: serde_json::from_str(&hardware).unwrap_or_default(),
                settings: serde_json::from_str(&settings).unwrap_or_default(),
                frames: serde_json::from_str(&frames).unwrap_or_default(),
                sections: Vec::new(),
            };
            Ok((PerfRow { version: r.get(0)?, platform: r.get(1)?, group }, hardware, settings))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut sections = db.prepare(
        "SELECT name, total_us, frames, worst_us FROM usage_perf_sections
         WHERE version = ? AND platform = ? AND hardware = ? AND settings = ? ORDER BY total_us DESC",
    )?;
    for (row, hardware, settings) in &mut rows {
        row.group.sections = sections
            .query_map(params![row.version, row.platform, &*hardware, &*settings], |r| {
                Ok(SectionStat {
                    name: r.get(0)?,
                    total_us: r.get::<_, i64>(1)? as u64,
                    frames: r.get::<_, i64>(2)? as u64,
                    worst_us: r.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    let mut rows: Vec<PerfRow> = rows.into_iter().map(|(row, ..)| row).collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.group.frames.iter().sum::<u64>()));
    Ok(rows)
}

/// Add a report's sessions and the lines the rules keep (the app should
/// have left the others out already); how many lines were kept.
fn add(db: &mut Connection, report: &UsageReport) -> rusqlite::Result<usize> {
    let rules: Vec<UsageRule> = parse_rules(&rules(db)?).unwrap_or_default();
    let now = now();
    let tx = db.transaction()?;
    if report.sessions > 0 {
        tx.execute(
            "INSERT INTO usage_sessions (version, platform, total, new) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT DO UPDATE SET total = total + ?3, new = new + ?3",
            params![report.version, report.platform, report.sessions as i64],
        )?;
    }
    let mut kept_lines = 0;
    for line in report.lines.iter().filter(|line| line.count > 0 && kept(&rules, line)) {
        tx.execute(
            "INSERT INTO usage (version, platform, level, target, signature, total, new, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?7)
             ON CONFLICT DO UPDATE SET total = total + ?6, new = new + ?6, last_seen = ?7",
            params![report.version, report.platform, line.level, line.target, line.signature, line.count as i64, now],
        )?;
        kept_lines += 1;
    }
    add_perf(&tx, report)?;
    tx.commit()?;
    Ok(kept_lines)
}

/// `WHERE` for the rows under `path`, with its values.
fn under(path: &[String]) -> (String, Vec<&str>) {
    let conditions: Vec<String> = LEVELS.iter().take(path.len()).map(|column| format!("{column} = ?")).collect();
    let clause = if conditions.is_empty() { "1".to_string() } else { conditions.join(" AND ") };
    (clause, path.iter().map(String::as_str).collect())
}

/// The level of the tree under `path`, biggest first.
pub(super) fn tree(db: &Connection, path: &[String]) -> Result<Vec<UsageNode>, String> {
    let Some(column) = LEVELS.get(path.len()) else { return Ok(Vec::new()) };
    let (clause, values) = under(path);
    let sql = format!("SELECT {column}, SUM(total), SUM(new) FROM usage WHERE {clause} GROUP BY {column} ORDER BY SUM(total) DESC");
    let mut nodes: Vec<UsageNode> = db
        .prepare(&sql)
        .and_then(|mut s| {
            s.query_map(rusqlite::params_from_iter(&values), |r| {
                Ok(UsageNode { name: r.get(0)?, total: r.get::<_, i64>(1)? as u64, new: r.get::<_, i64>(2)? as u64, sessions: None })
            })?
            .collect()
        })
        .map_err(|e| format!("database: {e}"))?;
    // (Sessions are per version and platform, so only those levels have them.)
    if path.len() < 2 {
        let sql = format!("SELECT SUM(total) FROM usage_sessions WHERE {clause} AND {column} = ?");
        for node in &mut nodes {
            let mut args = values.clone();
            args.push(&node.name);
            node.sessions = db
                .query_row(&sql, rusqlite::params_from_iter(&args), |r| r.get::<_, Option<i64>>(0))
                .map_err(|e| format!("database: {e}"))?
                .map(|n| n as u64);
        }
    }
    Ok(nodes)
}

/// Delete everything under `path`; how many rows went.
pub(super) fn clear(db: &Connection, path: &[String]) -> Result<usize, String> {
    let (clause, values) = under(path);
    let mut deleted = db
        .execute(&format!("DELETE FROM usage WHERE {clause}"), rusqlite::params_from_iter(&values))
        .map_err(|e| format!("database: {e}"))?;
    if path.len() <= 2 {
        deleted += db
            .execute(&format!("DELETE FROM usage_sessions WHERE {clause}"), rusqlite::params_from_iter(&values))
            .map_err(|e| format!("database: {e}"))?;
        for table in ["usage_perf", "usage_perf_sections"] {
            deleted += db
                .execute(&format!("DELETE FROM {table} WHERE {clause}"), rusqlite::params_from_iter(&values))
                .map_err(|e| format!("database: {e}"))?;
        }
    }
    Ok(deleted)
}

/// Replace the rules (checked first: none are replaced if one is wrong).
pub(super) fn set_rules(db: &mut Connection, set: &[String]) -> Result<(), String> {
    parse_rules(set)?;
    let set: Vec<String> = if set.is_empty() { DEFAULT_USAGE_RULES.map(String::from).to_vec() } else { set.to_vec() };
    let mut run = || -> rusqlite::Result<()> {
        let tx = db.transaction()?;
        tx.execute("DELETE FROM usage_rules", [])?;
        for rule in &set {
            tx.execute("INSERT INTO usage_rules (rule) VALUES (?)", [rule.split_whitespace().collect::<Vec<_>>().join(" ")])?;
        }
        tx.commit()
    };
    run().map_err(|e| format!("database: {e}"))
}

/// A signature without its line number, so a line keeps its key from
/// version to version: `synththing::sync:212 couldn't {}` is
/// `synththing::sync couldn't {}`, `src/app.rs:12: oops` is
/// `src/app.rs: oops`.
pub(super) fn key(signature: &str) -> String {
    let bytes = signature.as_bytes();
    let mut i = 0;
    while let Some(found) = signature[i..].find(':') {
        let at = i + found;
        let digits = bytes[at + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
        let after = bytes.get(at + 1 + digits);
        if digits > 0 && matches!(after, None | Some(b' ' | b':')) {
            return format!("{}{}", &signature[..at], &signature[at + 1 + digits..]);
        }
        i = at + 1;
    }
    signature.to_string()
}

/// One kind of line in intake's collection, across versions and platforms.
#[derive(Debug, Serialize, PartialEq)]
pub(super) struct DigestLine {
    pub level: String,
    pub target: String,
    /// What `taken` names it by.
    pub key: String,
    /// Its signatures (with line numbers), a few.
    pub signatures: Vec<String>,
    pub versions: Vec<String>,
    pub platforms: Vec<String>,
    /// Since intake last took it, and ever.
    pub new: u64,
    pub total: u64,
    pub first_seen: i64,
    /// The issue it was filed as, if it was: comment there at most, never
    /// file it again.
    pub filed: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(super) struct DigestSessions {
    pub version: String,
    pub platform: String,
    pub new: u64,
    pub total: u64,
}

#[derive(Deserialize)]
struct Filed {
    level: String,
    target: String,
    key: String,
    issue: i64,
}

#[derive(Deserialize)]
struct UsageTaken {
    #[serde(default)]
    filed: Vec<Filed>,
}

fn level_rank(level: &str) -> usize {
    USAGE_LEVELS.iter().position(|l| *l == level).unwrap_or(USAGE_LEVELS.len())
}

/// What's new, as intake gets it: panics first, then errors, warnings and
/// info, the most frequent first, at most `DIGEST_LINES`. The rows handed
/// out are marked offered, for `take`.
pub(super) fn digest(db: &mut Connection) -> rusqlite::Result<(Vec<DigestLine>, Vec<DigestSessions>)> {
    let tx = db.transaction()?;
    type Row = (String, String, String, String, String, u64, u64, i64);
    let rows: Vec<Row> = tx
        .prepare("SELECT version, platform, level, target, signature, total, new, first_seen FROM usage")?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get::<_, i64>(5)? as u64, r.get::<_, i64>(6)? as u64, r.get(7)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut groups: BTreeMap<(String, String, String), (DigestLine, Vec<usize>)> = BTreeMap::new();
    let mut sets: BTreeMap<(String, String, String), [BTreeSet<String>; 3]> = BTreeMap::new();
    for (i, (version, platform, level, target, signature, total, new, first_seen)) in rows.iter().enumerate() {
        let id = (level.clone(), target.clone(), key(signature));
        let (line, members) = groups.entry(id.clone()).or_insert_with(|| {
            (
                DigestLine {
                    level: level.clone(),
                    target: target.clone(),
                    key: id.2.clone(),
                    signatures: Vec::new(),
                    versions: Vec::new(),
                    platforms: Vec::new(),
                    new: 0,
                    total: 0,
                    first_seen: *first_seen,
                    filed: None,
                },
                Vec::new(),
            )
        });
        line.new += new;
        line.total += total;
        line.first_seen = line.first_seen.min(*first_seen);
        members.push(i);
        let [signatures, versions, platforms] = sets.entry(id).or_default();
        signatures.insert(signature.clone());
        versions.insert(version.clone());
        platforms.insert(platform.clone());
    }
    let mut lines: Vec<(DigestLine, Vec<usize>)> = groups
        .into_iter()
        .filter(|(_, (line, _))| line.new > 0)
        .map(|(id, (mut line, members))| {
            let [signatures, versions, platforms] = sets.remove(&id).unwrap_or_default();
            line.signatures = signatures.into_iter().take(DIGEST_EXAMPLES).collect();
            line.versions = versions.into_iter().rev().take(DIGEST_EXAMPLES).collect();
            line.platforms = platforms.into_iter().take(DIGEST_EXAMPLES).collect();
            (line, members)
        })
        .collect();
    lines.sort_by(|(a, _), (b, _)| level_rank(&a.level).cmp(&level_rank(&b.level)).then(b.new.cmp(&a.new)));
    lines.truncate(DIGEST_LINES);

    tx.execute("UPDATE usage SET offered = 0", [])?;
    for (line, members) in &mut lines {
        line.filed = tx
            .query_row(
                "SELECT issue FROM usage_filed WHERE level = ? AND target = ? AND key = ?",
                params![line.level, line.target, line.key],
                |r| r.get(0),
            )
            .optional()?;
        for &i in members.iter() {
            let (version, platform, level, target, signature, ..) = &rows[i];
            tx.execute(
                "UPDATE usage SET offered = new WHERE version = ? AND platform = ? AND level = ? AND target = ? AND signature = ?",
                params![version, platform, level, target, signature],
            )?;
        }
    }
    tx.execute("UPDATE usage_sessions SET offered = new", [])?;
    let sessions = tx
        .prepare("SELECT version, platform, new, total FROM usage_sessions WHERE new > 0 ORDER BY version DESC, platform")?
        .query_map([], |r| {
            Ok(DigestSessions { version: r.get(0)?, platform: r.get(1)?, new: r.get::<_, i64>(2)? as u64, total: r.get::<_, i64>(3)? as u64 })
        })?
        .collect::<rusqlite::Result<_>>()?;
    tx.commit()?;
    Ok((lines.into_iter().map(|(line, _)| line).collect(), sessions))
}

/// Intake took the last collection: what it was offered is no longer new,
/// and the lines it filed remember their issues.
fn take(db: &mut Connection, filed: &[Filed]) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    tx.execute("UPDATE usage SET new = new - offered, offered = 0", [])?;
    tx.execute("UPDATE usage_sessions SET new = new - offered, offered = 0", [])?;
    for f in filed {
        tx.execute(
            "INSERT INTO usage_filed (level, target, key, issue) VALUES (?, ?, ?, ?)
             ON CONFLICT DO UPDATE SET issue = excluded.issue",
            params![f.level, f.target, f.key, f.issue],
        )?;
    }
    tx.commit()
}

/// `GET /api/v1/intake/usage`: what's new in the usage data.
pub(super) fn intake(state: &State, request: &Request) -> Reply {
    if let Err(reply) = super::intake::authorized(state, request) {
        return reply;
    }
    match digest(&mut state.db()) {
        Ok((lines, sessions)) => json(200, &serde_json::json!({ "lines": lines, "sessions": sessions })),
        Err(e) => error(500, format!("database: {e}")),
    }
}

/// `POST /api/v1/intake/usage/taken`: `{"filed": [{level, target, key,
/// issue}]}`, the lines filed as (or added to) issues; the rest of what
/// was collected is taken too.
pub(super) fn intake_taken(state: &State, request: &mut Request) -> Reply {
    if let Err(reply) = super::intake::authorized(state, request) {
        return reply;
    }
    let body = match read_body(request, 256 * 1024) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let taken: UsageTaken = match serde_json::from_slice(&body) {
        Ok(taken) => taken,
        Err(e) => return error(400, format!("not a list of filed lines: {e}")),
    };
    match take(&mut state.db(), &taken.filed) {
        Ok(()) => {
            println!("intake took the usage data ({} lines filed)", taken.filed.len());
            json(200, &serde_json::json!({ "filed": taken.filed.len() }))
        }
        Err(e) => error(500, format!("database: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::usage::UsageLine;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA_V21).unwrap();
        db.execute_batch(SCHEMA_V22).unwrap();
        db.execute_batch(SCHEMA_V23).unwrap();
        db
    }

    #[test]
    fn frame_times_add_up_per_hardware_and_settings() {
        use crate::library::usage::{Hardware, PerfSettings};
        let mut db = db();
        let phone = Hardware { gpu: "Adreno 640".into(), backend: "Vulkan".into(), cores: 8, ram_gb: Some(8) };
        let group = |scale, frames: Vec<u64>, worst_us| PerfGroup {
            hardware: phone.clone(),
            settings: PerfSettings { mobile_layout: true, visualizer_scale: scale, window_kpx: 2500 },
            frames,
            sections: vec![SectionStat { name: "visualizer".into(), total_us: 9_000, frames: 1, worst_us }],
        };
        let mut sent = report("1.9.0", "android aarch64", &[]);
        sent.perf = vec![group(4, vec![1, 0, 0, 0, 0, 0], 9_000), group(1, vec![0, 0, 1, 0, 0, 0], 9_000)];
        assert!(check(&sent).is_ok());
        add(&mut db, &sent).unwrap();
        sent.perf = vec![group(4, vec![2, 1, 0, 0, 0, 0], 12_000)];
        add(&mut db, &sent).unwrap();

        let rows = perf(&db, Some("1.9.0")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].group.settings.visualizer_scale, rows[0].group.frames.clone()), (4, vec![3, 1, 0, 0, 0, 0]));
        assert_eq!(rows[0].group.sections, vec![SectionStat { name: "visualizer".into(), total_us: 18_000, frames: 2, worst_us: 12_000 }]);
        assert_eq!(rows[0].group.hardware, phone);
        assert!(perf(&db, Some("1.8.0")).unwrap().is_empty());

        clear(&db, &["1.9.0".into()]).unwrap();
        assert!(perf(&db, None).unwrap().is_empty());

        sent.perf[0].frames.pop();
        assert!(check(&sent).is_err(), "a count per bucket");
    }

    #[test]
    fn keys_leave_out_the_line_number() {
        assert_eq!(key("synththing::sync:212 couldn't send {name}: {e}"), "synththing::sync couldn't send {name}: {e}");
        assert_eq!(key("synththing::sync:212"), "synththing::sync");
        assert_eq!(key("src/app.rs:12: oops at 3:4"), "src/app.rs: oops at 3:4");
        assert_eq!(key("no line here: at all"), "no line here: at all");
    }

    #[test]
    fn intake_gets_whats_new_once_and_never_files_a_line_twice() {
        let mut db = db();
        let warn = |line: u32| ("warn", "synththing::sync", format!("synththing::sync:{line} couldn't send {{name}}"), 2);
        let (a, b) = (warn(10), warn(14));
        add(&mut db, &report("1.7.0", "linux x86_64", &[(a.0, a.1, &a.2, a.3), ("panic", "main", "src/app.rs:3: oops", 1)])).unwrap();
        add(&mut db, &report("1.8.0", "windows x86_64", &[(b.0, b.1, &b.2, b.3)])).unwrap();

        let (lines, sessions) = digest(&mut db).unwrap();
        assert_eq!(lines.len(), 2, "the warning in both versions is one line");
        assert_eq!((lines[0].level.as_str(), lines[0].new), ("panic", 1), "panics first");
        let warning = &lines[1];
        assert_eq!((warning.new, warning.versions.clone(), warning.filed), (4, vec!["1.8.0".to_string(), "1.7.0".to_string()], None));
        assert_eq!(sessions.iter().map(|s| s.new).sum::<u64>(), 4);

        // More comes in between collecting and taking: it stays new.
        add(&mut db, &report("1.8.0", "windows x86_64", &[(b.0, b.1, &b.2, 1)])).unwrap();
        let filed = Filed { level: "warn".into(), target: "synththing::sync".into(), key: warning.key.clone(), issue: 77 };
        take(&mut db, &[filed]).unwrap();
        let (lines, sessions) = digest(&mut db).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!((lines[0].new, lines[0].total, lines[0].filed), (1, 5, Some(77)), "known as #77 from now on");
        assert_eq!(sessions.iter().map(|s| s.new).sum::<u64>(), 2);

        take(&mut db, &[]).unwrap();
        assert!(digest(&mut db).unwrap().0.is_empty(), "nothing new: nothing to collect");
    }

    fn report(version: &str, platform: &str, lines: &[(&str, &str, &str, u64)]) -> UsageReport {
        UsageReport {
            perf: Vec::new(),
            version: version.into(),
            platform: platform.into(),
            sessions: 2,
            lines: lines
                .iter()
                .map(|&(level, target, signature, count)| UsageLine {
                    level: level.into(),
                    target: target.into(),
                    signature: signature.into(),
                    count,
                })
                .collect(),
        }
    }

    #[test]
    fn reports_add_up_into_the_tree() {
        let mut db = db();
        let windows = report("1.7.0", "windows x86_64", &[("warn", "synththing::sync", "synththing::sync:10", 3), ("info", "synththing::app", "synththing::app:5", 9)]);
        assert_eq!(add(&mut db, &windows).unwrap(), 1, "info is dropped by the default rules");
        add(&mut db, &windows).unwrap();
        add(&mut db, &report("1.7.0", "android aarch64", &[("panic", "main", "src/app.rs:3: oops", 1)])).unwrap();

        let versions = tree(&db, &[]).unwrap();
        assert_eq!(versions, vec![UsageNode { name: "1.7.0".into(), total: 7, new: 7, sessions: Some(6) }]);
        let platforms = tree(&db, &["1.7.0".into()]).unwrap();
        assert_eq!(platforms[0], UsageNode { name: "windows x86_64".into(), total: 6, new: 6, sessions: Some(4) });
        let signatures = tree(&db, &["1.7.0".into(), "windows x86_64".into(), "warn".into(), "synththing::sync".into()]).unwrap();
        assert_eq!(signatures, vec![UsageNode { name: "synththing::sync:10".into(), total: 6, new: 6, sessions: None }]);

        assert_eq!(clear(&db, &["1.7.0".into(), "windows x86_64".into()]).unwrap(), 2);
        assert_eq!(tree(&db, &[]).unwrap()[0].total, 1);
    }

    #[test]
    fn rules_can_be_changed() {
        let mut db = db();
        set_rules(&mut db, &["info  synththing::*   keep".into()]).unwrap();
        assert_eq!(rules(&db).unwrap(), vec!["info synththing::* keep"]);
        assert!(set_rules(&mut db, &["nope".into()]).is_err());
        assert_eq!(rules(&db).unwrap(), vec!["info synththing::* keep"], "a bad set changes nothing");
        set_rules(&mut db, &[]).unwrap();
        assert_eq!(rules(&db).unwrap(), DEFAULT_USAGE_RULES.to_vec());
    }

    #[test]
    fn bad_reports_are_refused() {
        assert!(check(&report("", "linux", &[])).is_err());
        assert!(check(&report("1.7.0", "linux", &[("debug", "x", "x:1", 1)])).is_err());
        assert!(check(&report("1.7.0", "linux", &[("warn", "x", "x:1", 1)])).is_ok());
    }
}

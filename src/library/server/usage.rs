//! Anonymous usage data (docs/USAGE.md): the counts apps that opted in
//! send, added up by version, platform, level, target and signature. No
//! address, key or ID is kept with them, so a row says how often something
//! happened, never to whom. Kept until an admin clears it
//! (`synththing admin usage --clear`); `new` counts what came in since
//! intake last took it.

use std::time::Duration;

use rusqlite::{params, Connection};
use tiny_http::Request;

use super::moderation::limit;
use super::{error, json, now, read_body, Reply, State};
use crate::library::usage::{
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::usage::UsageLine;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA_V21).unwrap();
        db
    }

    fn report(version: &str, platform: &str, lines: &[(&str, &str, &str, u64)]) -> UsageReport {
        UsageReport {
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

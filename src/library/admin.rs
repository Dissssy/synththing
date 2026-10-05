//! `synththing admin`: moderating a library server from the machine it
//! runs on. Each command is an `AdminAction` sent to the running server,
//! signed with the server's own key (`server.key` in its data folder), the
//! same requests an admin's app makes.

use crate::cli::{AdminArgs, AdminCommand};
use crate::library::identity::Identity;
use crate::library::{
    ago, client, key_id, lines_text, report_reason_title, AdminAction, AdminScriptInfo, Ban, Report,
};

pub fn run(args: &AdminArgs) -> Result<(), String> {
    let data = args.data.clone().unwrap_or_else(super::server::default_data_dir);
    let key_path = data.join("server.key");
    let key_text = std::fs::read_to_string(&key_path)
        .map_err(|e| format!("couldn't read {} ({e}): is --data the server's data folder?", key_path.display()))?;
    let server = Identity::from_secret_hex(&key_text).ok_or_else(|| format!("{} isn't a key", key_path.display()))?;
    let base = match &args.server {
        Some(url) => client::normalize_url(url),
        None => {
            let config = super::server::load_config(&data.join("server.json"))?;
            let bind = config.bind.replace("0.0.0.0", "127.0.0.1").replace("[::]", "[::1]");
            format!("http://{bind}")
        }
    };
    let key = server.public();
    let send = |action: AdminAction| client::admin::<serde_json::Value>(&base, &key, &server, &action);
    let done = |reply: serde_json::Value| {
        println!("{}", reply.get("done").and_then(|d| d.as_str()).unwrap_or("done"));
    };
    match &args.command {
        AdminCommand::Reports { all } => {
            let reports: Vec<Report> = client::admin(&base, &key, &server, &AdminAction::Reports { all: *all })?;
            if reports.is_empty() {
                println!("{}", if *all { "no reports" } else { "no open reports" });
            }
            for report in &reports {
                print_report(report);
            }
        }
        AdminCommand::Info { script } => {
            let info: AdminScriptInfo = client::admin(&base, &key, &server, &AdminAction::Info { script: script.clone() })?;
            print_info(&info);
        }
        AdminCommand::Hide { script, reason } => done(send(AdminAction::Hide { script: script.clone(), reason: reason.clone() })?),
        AdminCommand::Unhide { script } => done(send(AdminAction::Unhide { script: script.clone() })?),
        AdminCommand::Delete { script } => done(send(AdminAction::Delete { script: script.clone() })?),
        AdminCommand::Ban { target, reason, hours, hide_scripts } => {
            let reply = send(AdminAction::Ban {
                target: target.clone(),
                hours: *hours,
                reason: reason.clone(),
                hide_scripts: *hide_scripts,
            })?;
            let banned = reply["banned"].as_str().unwrap_or_default();
            let hidden = reply["scripts_hidden"].as_u64().unwrap_or(0);
            println!("banned {}{}", shown_target(banned), if hidden > 0 { format!(", {hidden} scripts hidden") } else { String::new() });
        }
        AdminCommand::Unban { target } => done(send(AdminAction::Unban { target: target.clone() })?),
        AdminCommand::Bans => {
            let bans: Vec<Ban> = client::admin(&base, &key, &server, &AdminAction::Bans)?;
            if bans.is_empty() {
                println!("no bans");
            }
            for ban in bans {
                let until = match ban.until {
                    Some(until) => format!("until {}", until_text(until)),
                    None => "for good".to_string(),
                };
                println!("{}  {until}  ({}): {}", shown_target(&ban.target), ago(ban.created), ban.reason);
            }
        }
        AdminCommand::AddAdmin { key: admin } => {
            let reply = send(AdminAction::AddAdmin { key: admin.clone() })?;
            println!("#{} is an admin", reply["admin"].as_str().unwrap_or_default());
        }
        AdminCommand::RemoveAdmin { key: admin } => done(send(AdminAction::RemoveAdmin { key: admin.clone() })?),
        AdminCommand::Admins => {
            let admins: Vec<(String, String)> = client::admin(&base, &key, &server, &AdminAction::Admins)?;
            if admins.is_empty() {
                println!("no admins (besides this command)");
            }
            for (hex, id) in admins {
                println!("#{id}  {hex}");
            }
        }
        AdminCommand::Resolve { report, note } => done(send(AdminAction::Resolve { report: *report, note: note.clone() })?),
    }
    Ok(())
}

/// A key as its ID (an address as it is).
fn shown_target(target: &str) -> String {
    if target.len() == 64 { format!("#{}", key_id(target)) } else { target.to_string() }
}

fn until_text(until: i64) -> String {
    let left = until - std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    format!("{} from now", crate::song_info::format_length(left.max(0) as f64))
}

fn print_report(report: &Report) {
    let state = match (&report.resolved, report.exists, report.hidden) {
        (Some(_), ..) => format!("  [resolved: {}]", report.resolution.as_deref().unwrap_or_default()),
        (None, false, _) => "  [script deleted]".to_string(),
        (None, true, true) => "  [script hidden]".to_string(),
        _ => String::new(),
    };
    println!(
        "#{}  {}  {} \"{}\" v{}: {}, by #{}{state}",
        report.id,
        ago(report.created),
        report.script_id,
        report.script_name,
        report.request.version,
        report_reason_title(&report.request.reason),
        report.reporter_id
    );
    if !report.request.details.is_empty() {
        for line in report.request.details.lines() {
            println!("    {line}");
        }
    }
    let mut marks = Vec::new();
    if !report.request.lines.is_empty() {
        marks.push(format!("lines {}", lines_text(&report.request.lines)));
    }
    if !report.request.sprites.is_empty() {
        let at: Vec<String> = report.request.sprites.iter().map(u32::to_string).collect();
        marks.push(format!("sprites at line {}", at.join(", ")));
    }
    if !marks.is_empty() {
        println!("    points at {}", marks.join("; "));
    }
}

fn print_info(info: &AdminScriptInfo) {
    let s = &info.summary;
    println!("{} \"{}\" ({}), by {}", s.id, s.name, s.category, s.author_name);
    match &info.author_key {
        Some(key) => println!("  key #{} {key}", key_id(key)),
        None => println!("  anonymous"),
    }
    if info.hidden {
        println!("  hidden: {}", info.hidden_reason.as_deref().unwrap_or_default());
    }
    println!("  {} encores", s.encores);
    for (version, ip, created) in &info.uploads {
        println!("  v{version}  {}  from {}", ago(*created), ip.as_deref().unwrap_or("(forgotten)"));
    }
    if info.reports.is_empty() {
        println!("  no reports");
    }
    for report in &info.reports {
        print_report(report);
    }
}

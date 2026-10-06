//! Email, for an authority (docs/SERVER.md): codes to confirm an address,
//! and recovery codes, sent through SendGrid's HTTP API (`ureq`, already
//! here for everything else) when `mail` is set in `server.json`, with the
//! API key read from a file in the data folder. Tests keep what would be
//! sent in an outbox instead.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use super::MailConfig;

/// An email to send: plain text and HTML.
#[derive(Clone, Debug)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

pub enum Mailer {
    Off,
    SendGrid { key: String, from: String, name: String },
    /// What would have been sent (tests).
    #[cfg_attr(not(test), expect(dead_code))]
    Outbox(Mutex<Vec<Mail>>),
}

impl Mailer {
    /// From the settings: SendGrid with the key in `api_key_file` (in
    /// `data` unless it's absolute), or off.
    pub fn new(config: Option<&MailConfig>, data: &Path) -> Result<Self, String> {
        let Some(config) = config else { return Ok(Self::Off) };
        #[cfg(test)]
        {
            let _ = (config, data);
            Ok(Self::Outbox(Mutex::default()))
        }
        #[cfg(not(test))]
        {
            let path = data.join(&config.api_key_file);
            let key = std::fs::read_to_string(&path)
                .map_err(|e| format!("couldn't read the SendGrid key from {}: {e}", path.display()))?;
            Ok(Self::SendGrid { key: key.trim().to_string(), from: config.from.clone(), name: config.from_name.clone() })
        }
    }

    pub fn on(&self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn send(&self, mail: Mail) -> Result<(), String> {
        match self {
            Self::Off => Err("this server doesn't send email".into()),
            Self::Outbox(outbox) => {
                outbox.lock().unwrap_or_else(|p| p.into_inner()).push(mail);
                Ok(())
            }
            Self::SendGrid { key, from, name } => {
                let body = serde_json::json!({
                    "personalizations": [{ "to": [{ "email": mail.to }] }],
                    "from": { "email": from, "name": name },
                    "subject": mail.subject,
                    "content": [
                        { "type": "text/plain", "value": mail.text },
                        { "type": "text/html", "value": mail.html },
                    ],
                    "tracking_settings": {
                        "click_tracking": { "enable": false, "enable_text": false },
                        "open_tracking": { "enable": false },
                    },
                });
                let agent: ureq::Agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .timeout_global(Some(Duration::from_secs(20)))
                    .build()
                    .into();
                let mut response = agent
                    .post("https://api.sendgrid.com/v3/mail/send")
                    .header("Authorization", format!("Bearer {key}"))
                    .send_json(&body)
                    .map_err(|e| format!("couldn't reach SendGrid: {e}"))?;
                let status = response.status().as_u16();
                if status < 300 {
                    return Ok(());
                }
                let said = response.body_mut().read_to_string().unwrap_or_default();
                println!("SendGrid said {status}: {said}");
                Err(format!("the email couldn't be sent (SendGrid said {status})"))
            }
        }
    }

    /// What's been "sent" (tests).
    #[cfg(test)]
    pub fn outbox(&self) -> Vec<Mail> {
        match self {
            Self::Outbox(outbox) => outbox.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            _ => Vec::new(),
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// An email with a code in it: `what` it's for ("to attach this address
/// to your identity"), and a link that cancels it, with what else that
/// does ("which also pauses ...").
pub fn code_mail(to: &str, server_name: &str, code: &str, what: &str, cancel: Option<(&str, &str)>) -> Mail {
    let subject = format!("Your synththing code: {code}");
    let mut text = format!(
        "Your code {what} on {server_name}:\n\n    {code}\n\nType it into synththing. It works for 30 minutes.\n\n\
         If you didn't ask for this, ignore this email: nothing changes without the code.\n"
    );
    let mut cancel_html = String::new();
    if let Some((link, also)) = cancel {
        text.push_str(&format!("\nThat wasn't you? Cancel it, {also}:\n{link}\n"));
        cancel_html = format!(
            "<p style=\"margin:24px 0 0;color:#555\">That wasn't you? <a href=\"{0}\" style=\"color:#2f6fde\">Cancel it</a>, \
             {1}.</p>",
            escape(link),
            escape(also)
        );
    }
    let html = format!(
        "<!doctype html><html><body style=\"margin:0;padding:24px;background:#f4f4f7;font-family:-apple-system,Segoe UI,\
         Helvetica,Arial,sans-serif;color:#1d1d24\">\
         <div style=\"max-width:480px;margin:0 auto;background:#ffffff;border-radius:8px;padding:28px\">\
         <div style=\"font-size:15px;font-weight:600;letter-spacing:0.02em;color:#6b5bd6\">synththing</div>\
         <p style=\"margin:16px 0 8px\">Your code {what} on {server}:</p>\
         <div style=\"font-family:Consolas,Menlo,monospace;font-size:30px;font-weight:700;letter-spacing:0.12em;\
         padding:14px 0\">{code}</div>\
         <p style=\"margin:8px 0 0\">Type it into synththing. It works for 30 minutes.</p>\
         <p style=\"margin:16px 0 0;color:#555\">If you didn't ask for this, ignore this email: nothing changes \
         without the code.</p>{cancel_html}</div></body></html>",
        what = escape(what),
        server = escape(server_name),
        code = escape(code),
    );
    Mail { to: to.to_string(), subject, text, html }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends the real emails through SendGrid, to see how they look.
    /// `SYNTHTHING_TEST_MAIL_KEY` names a file with the API key,
    /// `SYNTHTHING_TEST_MAIL_FROM` and `SYNTHTHING_TEST_MAIL_TO` the
    /// addresses: `cargo test --bin synththing sends_real_emails -- --ignored`.
    #[test]
    #[ignore = "sends real email"]
    fn sends_real_emails() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let key = std::fs::read_to_string(var("SYNTHTHING_TEST_MAIL_KEY")).unwrap().trim().to_string();
        let to = var("SYNTHTHING_TEST_MAIL_TO");
        let mailer = Mailer::SendGrid { key, from: var("SYNTHTHING_TEST_MAIL_FROM"), name: "synththing".into() };
        let server = "synththing official library";
        let cancel = "https://synththing.p51.nl/api/v1/identity/cancel?token=0123456789abcdef";
        let attach = (cancel, "and this address won't be attached (whoever asked can't try again for a day)");
        mailer.send(code_mail(&to, server, "K3F9-Q2XA", "to attach this address to your identity", Some(attach))).unwrap();
        let recover = (cancel, "which also pauses recovery of your identity for a day");
        mailer.send(code_mail(&to, server, "B7RD-M4WZ", "to recover your identity (with a new key)", Some(recover))).unwrap();
    }
}

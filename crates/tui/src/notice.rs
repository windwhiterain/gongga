//! A toast on the desktop, so a session that stops says so outside the window.
//!
//! # Invariant
//!
//! The toast is a courtesy, never a duty: a window that cannot show one still
//! works, and a toast that fails says so instead of holding the window up.
//! See `docs/notes/tui-notice.md`.

/// The toast as the notification centre shows it: the name and the sentence.
pub fn toast(title: &str, body: &str) -> anyhow::Result<()> {
    show(title, body)
}

/// Shows the toast, where this machine can show one.
///
/// # Invariant
///
/// Nothing is waited for: the window draws its next frame while the toast is on
/// its way, so a slow shell cannot hold the window up.
#[cfg(windows)]
fn show(title: &str, body: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let script = script(title, body);
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encode(&script),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("starting powershell for the toast")?;
    Ok(())
}

/// A machine with no toast to show: the window says nothing and carries on.
#[cfg(not(windows))]
fn show(_title: &str, _body: &str) -> anyhow::Result<()> {
    Ok(())
}

/// The PowerShell that shows the toast, as one script.
///
/// # Invariant
///
/// The title and the body go in as escaped text, so a name holding a quote or
/// an ampersand cannot break the script or the XML it carries.
#[cfg(windows)]
fn script(title: &str, body: &str) -> String {
    let xml = format!(
        "<toast><visual><binding template=\"ToastText02\"><text id=\"1\">{}</text>\
         <text id=\"2\">{}</text></binding></visual></toast>",
        escape(title),
        escape(body)
    );
    let load = format!("$xml.LoadXml('{}')", literal(&xml));
    let show = format!(
        "[Windows.UI.Notifications.ToastNotificationManager]::\
         CreateToastNotifier('{}').Show($toast)",
        AUMID
    );
    [
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
         ContentType = WindowsRuntime] | Out-Null",
        "[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom, \
         ContentType = WindowsRuntime] | Out-Null",
        "$xml = New-Object Windows.Data.Xml.Dom.XmlDocument",
        load.as_str(),
        "$toast = New-Object Windows.UI.Notifications.ToastNotification $xml",
        show.as_str(),
    ]
    .join("\n")
}

/// The name the notification centre shows the toast under.
///
/// # Invariant
///
/// A name of its own, registered nowhere: Windows shows the string itself for
/// an application it does not know, and the toast carries no file and no
/// registry entry of its own.
#[cfg(windows)]
const AUMID: &str = "gongga";

/// XML text, escaped: the toast's own text is data, never markup.
#[cfg(windows)]
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A PowerShell single-quoted string: an embedded quote is doubled.
#[cfg(windows)]
fn literal(text: &str) -> String {
    text.replace('\'', "''")
}

/// The script as `-EncodedCommand` wants it: UTF-16LE, base64.
///
/// # Invariant
///
/// UTF-16LE, because that is what the flag decodes: a script in any other
/// encoding is read as noise, and a name with a character outside ASCII is lost.
#[cfg(windows)]
fn encode(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

//! Desktop notifications ("Download complete"), sent straight to the notification service on the
//! session bus. The legacy app ran `notify-send` found through `PATH`; this needs no helper
//! program and nothing is looked up in a directory somebody else might write to.

use std::collections::HashMap;

use gtk4::gio::{self, prelude::*};
use gtk4::glib;

use crate::log;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const APP_NAME: &str = "llm-wrapper";
/// Most characters of the summary and of the body we hand to the service.
const MAX_CHARS: usize = 200;

/// Text safe to give a notification service: no control characters, no markup (servers may
/// interpret `<b>` and `<a href>` in the body), and bounded.
pub fn plain_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()).take(MAX_CHARS) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// Show a notification; failures (no service on the bus, a bus that is gone) are only logged.
pub fn send(summary: &str, body: &str) {
    let parameters = (
        APP_NAME,
        0_u32,
        "",
        plain_text(summary),
        plain_text(body),
        Vec::<String>::new(),
        HashMap::<String, glib::Variant>::new(),
        -1_i32,
    )
        .to_variant();
    glib::spawn_future_local(async move {
        let result = async {
            let bus = gio::bus_get_future(gio::BusType::Session).await?;
            bus.call_future(
                Some(SERVICE),
                PATH,
                SERVICE,
                "Notify",
                Some(&parameters),
                None,
                gio::DBusCallFlags::NONE,
                2000,
            )
            .await
        }
        .await;
        if let Err(e) = result {
            log::warn(&format!("could not show a notification: {}", e.message()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_and_control_characters_do_not_reach_the_service() {
        assert_eq!(
            plain_text("Saved <b>x</b> & y"),
            "Saved &lt;b&gt;x&lt;/b&gt; &amp; y"
        );
        assert_eq!(plain_text("a\u{1b}[31mred\nline\u{7}"), "a[31mredline");
        assert_eq!(plain_text(&"x".repeat(1000)).len(), MAX_CHARS);
        assert_eq!(plain_text(""), "");
    }

    #[test]
    fn the_call_has_the_signature_the_specification_requires() {
        // org.freedesktop.Notifications.Notify (susssasa{sv}i)
        let v = (
            APP_NAME,
            0_u32,
            "",
            "s",
            "b",
            Vec::<String>::new(),
            HashMap::<String, glib::Variant>::new(),
            -1_i32,
        )
            .to_variant();
        assert_eq!(v.type_().as_str(), "(susssasa{sv}i)");
    }
}

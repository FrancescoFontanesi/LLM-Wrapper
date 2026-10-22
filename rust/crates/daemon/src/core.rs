//! What a command means, independent of GTK.
//!
//! [`plan`] turns the current [`Model`] and a [`Request`] into a [`Plan`] — or refuses it, before
//! anything has happened. Keeping this pure makes the whole behaviour of the CLI testable without a
//! display, and guarantees a refused command has no side effects.

use std::collections::BTreeMap;

use llm_wrapper_protocol::{Flags, ProviderRef, Request};

/// The provider used when none was asked for.
pub const DEFAULT_PROVIDER: &str = "chatgpt";
/// Minutes hidden before the memory trim, unless `--idle-timeout` says otherwise.
pub const DEFAULT_IDLE_MINUTES: u32 = 15;

/// Provider name to URL, sorted by name.
pub type Providers = BTreeMap<String, String>;

/// The providers used until a list is saved, and whenever the saved list is unusable.
pub fn builtin_providers() -> Providers {
    [
        ("chatgpt", "https://chatgpt.com"),
        ("gemini", "https://gemini.google.com"),
        ("claude", "https://claude.ai"),
    ]
    .into_iter()
    .map(|(n, u)| (n.to_owned(), u.to_owned()))
    .collect()
}

/// The provider whose page is loaded. Kept as a copy so that removing it from the list does not
/// pull the page from under the user (it goes away at the next restart, as before).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Active {
    /// Provider name, or the URL itself for an ad-hoc provider.
    pub name: String,
    /// What is loaded.
    pub url: String,
}

/// What the commands act on.
#[derive(Clone, Debug)]
pub struct Model {
    /// The provider list (never empty).
    pub providers: Providers,
    /// `None` until the first page is chosen at start-up.
    pub active: Option<Active>,
}

/// What to do with the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    /// Show and focus it.
    Show,
    /// Hide it.
    Hide,
    /// Flip it.
    Toggle,
    /// Leave it as it is.
    #[default]
    Keep,
}

/// Everything a command will do, decided and validated.
// Independent switches that the request itself carries as flags; an enum would only re-encode them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Shut down; nothing else is done.
    pub quit: bool,
    /// New idle timeout in minutes.
    pub idle_minutes: Option<u32>,
    /// New User-Agent.
    pub user_agent: Option<String>,
    /// The provider list to persist, if it changed.
    pub providers: Option<Providers>,
    /// Provider to make active.
    pub activate: Option<Active>,
    /// Load the active provider's root again.
    pub new_chat: bool,
    /// Release the memory cache.
    pub soft_clean: bool,
    /// Purge disk and DOM caches.
    pub hard_clean: bool,
    /// What happens to the window.
    pub visibility: Visibility,
}

fn default_active(providers: &Providers) -> Active {
    let (name, url) = providers
        .get_key_value(DEFAULT_PROVIDER)
        .or_else(|| providers.iter().next())
        .expect("the provider list is never empty");
    Active {
        name: name.clone(),
        url: url.clone(),
    }
}

/// Decide what `request` does. `Err` carries the message for the user; nothing has been changed.
///
/// Order, as in the legacy app: `--quit` first (and nothing else then); provider edits (add, then
/// remove); the provider switch; the actions; finally visibility.
pub fn plan(model: &Model, request: &Request) -> Result<Plan, String> {
    let flags = request.flags;
    if flags.contains(Flags::QUIT) {
        return Ok(Plan {
            quit: true,
            ..Plan::default()
        });
    }

    let mut providers = model.providers.clone();
    if let Some((name, url)) = &request.add_provider {
        providers.insert(name.to_string(), url.to_string());
    }
    if let Some(name) = &request.remove_provider {
        if providers.remove(name.as_str()).is_none() {
            return Err(format!("unknown provider: {name}"));
        }
        if providers.is_empty() {
            return Err("cannot remove the last provider".to_owned());
        }
    }

    let activate = match &request.provider {
        Some(ProviderRef::Name(name)) => {
            let url = providers
                .get(name.as_str())
                .ok_or_else(|| format!("unknown provider: {name}"))?;
            Some(Active {
                name: name.to_string(),
                url: url.clone(),
            })
        }
        Some(ProviderRef::Url(url)) => Some(Active {
            name: url.to_string(),
            url: url.to_string(),
        }),
        None if model.active.is_none() => Some(default_active(&providers)),
        None => None,
    };

    // A command that names no action at all (a bare `llm-wrapper`, or only `--instance`) toggles.
    let visibility = if flags.contains(Flags::TOGGLE) || *request == Request::default() {
        Visibility::Toggle
    } else if flags.contains(Flags::SHOW) {
        Visibility::Show
    } else if flags.contains(Flags::HIDE) || flags.contains(Flags::BACKGROUND) {
        Visibility::Hide
    } else if flags.contains(Flags::NEW_CHAT) || request.provider.is_some() {
        // `--new-chat` starts a chat you are about to read; the README says `--provider` shows the window
        Visibility::Show
    } else {
        Visibility::Keep
    };

    Ok(Plan {
        quit: false,
        idle_minutes: request.idle_timeout_minutes,
        user_agent: request.user_agent.as_ref().map(|u| u.as_str().to_owned()),
        providers: (providers != model.providers).then_some(providers),
        activate,
        new_chat: flags.contains(Flags::NEW_CHAT),
        soft_clean: flags.contains(Flags::SOFT_CLEAN),
        hard_clean: flags.contains(Flags::HARD_CLEAN),
        visibility,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_wrapper_protocol::{parse_args, Parsed};

    fn req(args: &[&str]) -> Request {
        let v: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        match parse_args(&v).expect("valid command line") {
            Parsed::Run(i) => i.request,
            other => panic!("{other:?}"),
        }
    }

    fn running() -> Model {
        Model {
            providers: builtin_providers(),
            active: Some(Active {
                name: "chatgpt".into(),
                url: "https://chatgpt.com".into(),
            }),
        }
    }

    fn fresh() -> Model {
        Model {
            active: None,
            ..running()
        }
    }

    fn vis(args: &[&str]) -> Visibility {
        plan(&running(), &req(args)).unwrap().visibility
    }

    #[test]
    fn quit_wins_and_does_nothing_else() {
        let p = plan(
            &running(),
            &req(&[
                "--quit",
                "--provider",
                "gemini",
                "--show",
                "--add-provider",
                "x",
                "https://x.example",
            ]),
        )
        .unwrap();
        assert_eq!(
            p,
            Plan {
                quit: true,
                ..Plan::default()
            }
        );
        // even a command that would have been refused
        assert!(
            plan(&running(), &req(&["--quit", "--provider", "no-such"]))
                .unwrap()
                .quit
        );
    }

    #[test]
    fn visibility_precedence() {
        assert_eq!(vis(&["--toggle", "--show"]), Visibility::Toggle);
        assert_eq!(vis(&["--hide", "--show"]), Visibility::Show);
        assert_eq!(vis(&["--hide", "--background"]), Visibility::Hide);
        assert_eq!(vis(&["--background"]), Visibility::Hide);
        assert_eq!(vis(&["--show", "--new-chat"]), Visibility::Show);
        assert_eq!(
            vis(&["--hide", "--new-chat"]),
            Visibility::Hide,
            "an explicit flag wins over the default"
        );
        assert_eq!(
            vis(&["--background", "--provider", "gemini"]),
            Visibility::Hide
        );
    }

    #[test]
    fn only_new_chat_and_provider_show_the_window_by_themselves() {
        assert_eq!(vis(&["--new-chat"]), Visibility::Show);
        assert_eq!(vis(&["--provider", "gemini"]), Visibility::Show);
        assert_eq!(vis(&["--provider", "https://x.example"]), Visibility::Show);
        for args in [
            &["--soft-clean"][..],
            &["--hard-clean"],
            &["--idle-timeout", "5"],
            &["--user-agent", "UA/1"],
            &["--add-provider", "x", "https://x.example"],
            &["--remove-provider", "claude"],
        ] {
            assert_eq!(vis(args), Visibility::Keep, "{args:?}");
        }
    }

    #[test]
    fn a_command_with_no_action_toggles() {
        assert_eq!(vis(&[]), Visibility::Toggle);
        assert_eq!(vis(&["--instance", "work"]), Visibility::Toggle);
    }

    #[test]
    fn a_fresh_daemon_picks_the_default_provider() {
        let p = plan(&fresh(), &req(&["--background"])).unwrap();
        assert_eq!(
            p.activate,
            Some(Active {
                name: "chatgpt".into(),
                url: "https://chatgpt.com".into()
            })
        );
        let p = plan(&fresh(), &req(&["--hard-clean"])).unwrap();
        assert!(
            p.activate.is_some(),
            "even a maintenance command must leave a page loaded"
        );
        assert_eq!(p.visibility, Visibility::Keep, "...but hidden");
    }

    #[test]
    fn the_default_falls_back_to_the_first_provider_when_chatgpt_is_not_listed() {
        let mut m = fresh();
        m.providers.remove("chatgpt");
        let p = plan(&m, &req(&["--background"])).unwrap();
        assert_eq!(p.activate.unwrap().name, "claude");
    }

    #[test]
    fn a_running_daemon_keeps_its_provider_unless_told_otherwise() {
        assert_eq!(plan(&running(), &req(&["--show"])).unwrap().activate, None);
        let p = plan(&running(), &req(&["--provider", "gemini"])).unwrap();
        assert_eq!(
            p.activate,
            Some(Active {
                name: "gemini".into(),
                url: "https://gemini.google.com".into()
            })
        );
        let p = plan(&running(), &req(&["--provider", "http://localhost:3000/x"])).unwrap();
        assert_eq!(
            p.activate,
            Some(Active {
                name: "http://localhost:3000/x".into(),
                url: "http://localhost:3000/x".into()
            })
        );
    }

    #[test]
    fn unknown_providers_are_refused_without_side_effects() {
        let e = plan(
            &running(),
            &req(&[
                "--provider",
                "nope",
                "--add-provider",
                "x",
                "https://x.example",
                "--show",
            ]),
        )
        .unwrap_err();
        assert_eq!(e, "unknown provider: nope");
        assert_eq!(
            plan(&running(), &req(&["--remove-provider", "nope"])).unwrap_err(),
            "unknown provider: nope"
        );
    }

    #[test]
    fn a_provider_can_be_added_and_used_in_one_command() {
        let p = plan(
            &running(),
            &req(&[
                "--add-provider",
                "mistral",
                "https://chat.mistral.ai",
                "--provider",
                "mistral",
            ]),
        )
        .unwrap();
        assert_eq!(p.activate.unwrap().url, "https://chat.mistral.ai");
        assert_eq!(p.providers.unwrap()["mistral"], "https://chat.mistral.ai");
    }

    #[test]
    fn add_then_remove_and_replace() {
        let p = plan(
            &running(),
            &req(&[
                "--add-provider",
                "x",
                "https://x.example",
                "--remove-provider",
                "x",
            ]),
        )
        .unwrap();
        assert_eq!(
            p.providers, None,
            "added and removed again: nothing to save"
        );
        let p = plan(
            &running(),
            &req(&["--add-provider", "chatgpt", "https://other.example"]),
        )
        .unwrap();
        assert_eq!(p.providers.unwrap()["chatgpt"], "https://other.example");
        // adding what is already there changes nothing
        assert_eq!(
            plan(
                &running(),
                &req(&["--add-provider", "chatgpt", "https://chatgpt.com"])
            )
            .unwrap()
            .providers,
            None
        );
    }

    #[test]
    fn the_list_never_becomes_empty() {
        let mut m = running();
        m.providers = [("only".to_owned(), "https://only.example".to_owned())]
            .into_iter()
            .collect();
        assert_eq!(
            plan(&m, &req(&["--remove-provider", "only"])).unwrap_err(),
            "cannot remove the last provider"
        );
    }

    #[test]
    fn removing_the_active_provider_does_not_change_the_active_page() {
        let p = plan(&running(), &req(&["--remove-provider", "chatgpt"])).unwrap();
        assert_eq!(p.activate, None);
        assert!(!p.providers.unwrap().contains_key("chatgpt"));
    }

    #[test]
    fn actions_and_settings_are_carried_over() {
        let p = plan(
            &running(),
            &req(&[
                "--new-chat",
                "--soft-clean",
                "--hard-clean",
                "--idle-timeout",
                "7",
                "--user-agent",
                "UA/2",
            ]),
        )
        .unwrap();
        assert!(p.new_chat && p.soft_clean && p.hard_clean);
        assert_eq!(
            (p.idle_minutes, p.user_agent.as_deref()),
            (Some(7), Some("UA/2"))
        );
        assert!(!plan(&running(), &req(&["--show"])).unwrap().new_chat);
    }
}

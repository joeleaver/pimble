//! Who is here, drawn (docs/PRESENCE_CONTRACT.md section 6): a person's
//! initials in their color on a pane's strip, an agent's robot beside its
//! initials, a dot on a tree row, and the words a hover says. The app and
//! plugins draw presence with these components, so a person in a plugin's
//! view looks exactly as they do in a native one.
//!
//! Nothing here knows where a person is or what they are called: whoever
//! shows them resolves that (the app from its presence and the names in each
//! share, a plugin from what the host hands it) and passes a [`Person`].

use rinch::core::InputCallback;
use rinch::prelude::*;

use crate::appearance::IconGlyph;

/// A person in a node, their sessions merged: what an initials chip and a
/// row's dot show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    /// What every session of theirs shares: their account, an agent apart
    /// from its account, a session when signed out. Following names it.
    pub key: String,
    pub name: String,
    /// One or two letters ([`initials`]).
    pub initials: String,
    /// `#rrggbb`: the account's author color, the same on every device.
    pub color: String,
    /// Name and devices, for the hover ([`hover`]).
    pub title: String,
    /// An AI agent: a robot beside its initials, and never followed.
    pub agent: bool,
}

/// One or two letters for a name: the first of its first two words.
pub fn initials(name: &str) -> String {
    let letters: String = name.split_whitespace().take(2).filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).flat_map(char::to_uppercase).collect();
    if letters.is_empty() {
        "?".to_string()
    } else {
        letters
    }
}

/// What hovering someone says: their name and the devices they are on, or
/// for an agent, that it is one and the app it works through.
pub fn hover(name: &str, agent: bool, devices: &[String]) -> String {
    let devices: Vec<&str> = devices.iter().map(String::as_str).filter(|d| !d.is_empty()).collect();
    match (agent, devices.is_empty()) {
        (true, true) => format!("{name}, an AI agent"),
        (true, false) => format!("{name}, an AI agent in {}", devices.join(", ")),
        (false, true) => name.to_string(),
        (false, false) => format!("{name}, on {}", devices.join(", ")),
    }
}

/// What a hover adds about following: nothing for an agent, which cannot be
/// followed (docs/PRESENCE_CONTRACT.md section 8).
pub fn follow_hint(agent: bool) -> &'static str {
    if agent {
        ""
    } else {
        " (click to follow)"
    }
}

/// What a row of chips says for the people it had no room for.
pub fn more_label(more: usize) -> String {
    format!("+{more}")
}

/// A person's initials in their color, as a pane's strip shows them. A click
/// asks to follow them (`onfollow` with their key); an agent's chip carries
/// a robot and follows nobody.
#[component]
pub fn PersonChip(person: Person, onfollow: Option<InputCallback>) -> NodeHandle {
    let agent = person.agent;
    let key = person.key.clone();
    rsx! {
        span {
            class: if agent { "pimble-pane__person pimble-pane__person--agent" } else { "pimble-pane__person" },
            title: format!("{}{}", person.title, follow_hint(agent)),
            style: format!("background: {};", person.color),
            onclick: move || {
                if !agent {
                    if let Some(follow) = &onfollow {
                        follow.invoke(key.clone());
                    }
                }
            },
            // An agent's initials carry a robot.
            span {
                class: "pimble-presence-robot",
                style: if agent { "" } else { "display: none;" },
                IconGlyph { name: "robot".to_string() }
            }
            {person.initials.clone()}
        }
    }
}

/// A person's dot, as a tree row shows them: a small disc in their color, or
/// for an agent a robot in its color.
#[component]
pub fn PersonDot(person: Person) -> NodeHandle {
    let agent = person.agent;
    rsx! {
        span {
            class: if agent { "pimble-tree-people__dot pimble-tree-people__dot--agent" } else { "pimble-tree-people__dot" },
            title: person.title.clone(),
            style: if agent { format!("color: {};", person.color) } else { format!("background: {};", person.color) },
            span {
                class: "pimble-presence-robot",
                style: if agent { "" } else { "display: none;" },
                IconGlyph { name: "robot".to_string() }
            }
        }
    }
}

/// The stylesheet for [`PersonChip`] and [`PersonDot`] and the rows that hold
/// them (`pimble-pane__people`, `pimble-pane__more`, `pimble-tree-people`).
pub const STYLES: &str = r#"
/* ── Presence (pimble-ui presence.rs) ────────────────────────────── */

.pimble-pane__people {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: 6px;
}

.pimble-pane__person {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 9px;
    font-size: 9px;
    font-weight: 600;
    color: var(--rinch-color-white, #ffffff);
    cursor: pointer;
}

.pimble-pane__more {
    font-size: 11px;
    color: var(--rinch-color-dimmed);
}

.pimble-tree-people {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: 4px;
}

.pimble-tree-people__dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 3px;
}

/* An AI agent (docs/PRESENCE_CONTRACT.md section 8): a small robot beside
   its initials on the pane and its caret's flag, and in place of its dot. */
.pimble-presence-robot {
    display: inline-flex;
    width: 10px;
    height: 10px;
    flex-shrink: 0;
}

.pimble-pane__person--agent {
    width: auto;
    padding: 0 5px 0 4px;
    gap: 2px;
    cursor: default;
}

.pimble-tree-people__dot--agent {
    display: inline-flex;
    width: 11px;
    height: 11px;
    border-radius: 0;
}

.pimble-tree-people__dot--agent .pimble-presence-robot {
    width: 11px;
    height: 11px;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_named_as_one_and_not_offered_to_follow() {
        assert_eq!(hover("Claude (Sam's agent)", true, &["Claude Code".to_string()]), "Claude (Sam's agent), an AI agent in Claude Code");
        assert_eq!(hover("Claude", true, &[String::new()]), "Claude, an AI agent");
        assert_eq!(hover("Sam", false, &["laptop".to_string(), "Web".to_string()]), "Sam, on laptop, Web");
        assert_eq!(follow_hint(true), "");
        assert_eq!(follow_hint(false), " (click to follow)");
        assert_eq!(more_label(3), "+3");
    }

    #[test]
    fn initials_take_the_first_letter_of_two_words() {
        assert_eq!(initials("Chef Ann"), "CA");
        assert_eq!(initials("bob"), "B");
        assert_eq!(initials("  mary   jane watson "), "MJ");
        assert_eq!(initials("(Grandma)"), "G");
        assert_eq!(initials(""), "?");
    }
}

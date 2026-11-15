//! E-mail screens: the services column beside every tab; Messages, Inboxes, Extractors, Settings.

pub mod area;
pub mod extractors;
pub mod inboxes;
pub mod mailboxes;
pub mod messages;
pub mod service;

use leptos::prelude::*;

/// E3 settings / E4 extractors: the shared e-mail header.
#[component]
pub fn ServicesHead(active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    view! { <area::EmailHead active=if active == "services" { "settings" } else { active } right=right/> }
}

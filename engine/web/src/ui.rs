//! Shared controls (X1–X3 interaction sheets): toggles, segmented controls, menus, dialogs, toasts.

use leptos::prelude::*;

#[derive(Clone, Copy)]
pub struct Toasts(pub RwSignal<Vec<(u32, String, bool)>>);

pub fn toast(msg: impl Into<String>) {
    push(msg.into(), false);
}

pub fn toast_err(msg: impl Into<String>) {
    push(msg.into(), true);
}

fn push(msg: String, bad: bool) {
    let Some(t) = use_context::<Toasts>() else { return };
    let id = (js_sys::Math::random() * 1e9) as u32;
    t.0.update(|v| v.push((id, msg, bad)));
    set_timeout(move || t.0.update(|v| v.retain(|x| x.0 != id)), std::time::Duration::from_millis(if bad { 6000 } else { 3000 }));
}

#[component]
pub fn ToastHost() -> impl IntoView {
    let t = expect_context::<Toasts>();
    view! {
        <div class="toasts">
            {move || t.0.get().into_iter().map(|(_, m, bad)| view! { <div class="toast" class:bad=bad>{m}</div> }).collect_view()}
        </div>
    }
}

pub fn copy(text: &str, what: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.navigator().clipboard().write_text(text);
    }
    toast(format!("{what} copied"));
}

#[component]
pub fn Toggle(#[prop(into)] on: Signal<bool>, #[prop(into)] set: Callback<bool>, #[prop(optional)] disabled: bool) -> impl IntoView {
    view! { <button class="toggle" class:on=move || on.get() disabled=disabled on:click=move |_| set.run(!on.get_untracked())></button> }
}

#[component]
pub fn Seg(options: Vec<(&'static str, &'static str)>, #[prop(into)] value: Signal<String>, #[prop(into)] set: Callback<String>) -> impl IntoView {
    view! {
        <div class="seg">
            {options.into_iter().map(|(v, label)| view! {
                <button class:on=move || value.get() == v on:click=move |_| set.run(v.to_string())>{label}</button>
            }).collect_view()}
        </div>
    }
}

/// A dropdown that closes on any click outside, and on item click unless `keep`.
#[component]
pub fn Pop(open: RwSignal<bool>, #[prop(optional)] right: bool, #[prop(optional)] keep: bool, children: ChildrenFn) -> impl IntoView {
    let children = StoredValue::new(children);
    view! {
        <Show when=move || open.get()>
            <div style="position:fixed;inset:0;z-index:29" on:click=move |_| open.set(false)></div>
            <div class="menu" class:right=right on:click=move |_| if !keep { open.set(false) }>{children.with_value(|c| c())}</div>
        </Show>
    }
}

#[component]
pub fn Dialog(open: RwSignal<bool>, title: &'static str, children: ChildrenFn) -> impl IntoView {
    esc_closes(open);
    let children = StoredValue::new(children);
    view! {
        <Show when=move || open.get()>
            <div class="scrim" on:click=move |_| open.set(false)>
                <div class="dialog" on:click=|e| e.stop_propagation()>
                    // An empty title: the dialog draws its own heading (it changes with what it does).
                    {(!title.is_empty()).then(|| view! { <h2><span>{title}</span><button class="link plain" on:click=move |_| open.set(false)>"×"</button></h2> })}
                    {children.with_value(|c| c())}
                </div>
            </div>
        </Show>
    }
}

/// "Delete this inbox and its 3 messages?" with Cancel / Delete.
#[component]
pub fn Confirm(open: RwSignal<bool>, #[prop(into)] question: Signal<String>, #[prop(into)] detail: Signal<String>, action: &'static str, #[prop(into)] on_yes: Callback<()>) -> impl IntoView {
    esc_closes(open);
    view! {
        <Show when=move || open.get()>
            <div class="scrim" on:click=move |_| open.set(false)>
                <div class="dialog" on:click=|e| e.stop_propagation()>
                    <h2>{move || question.get()}</h2>
                    <div class="t2">{move || detail.get()}</div>
                    <div class="actions">
                        <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button>
                        <button class="btn danger solid" on:click=move |_| { open.set(false); on_yes.run(()) }>{action}</button>
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// X2: Esc cancels an open dialog (clicking outside already does).
fn esc_closes(open: RwSignal<bool>) {
    let h = window_event_listener(leptos::ev::keydown, move |e| if e.key() == "Escape" && open.get_untracked() { open.set(false) });
    on_cleanup(move || h.remove());
}

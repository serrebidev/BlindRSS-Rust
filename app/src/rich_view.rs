//! A stable WebView document with coalesced, deferred article replacements.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::Rc;
use wxdragon::event::webview_events::WebViewEvents;
use wxdragon::timer::Timer;
use wxdragon::widgets::webview::WebView;

#[derive(Clone)]
pub(crate) struct RichView {
    view: WebView,
    state: Rc<RefCell<RenderState>>,
    rendered: Rc<RefCell<Option<RenderCallback>>>,
    notify_pending: Rc<Cell<bool>>,
    timer: Rc<Timer<WebView>>,
}

type RenderCallback = Rc<dyn Fn()>;

impl RichView {
    pub(crate) fn new(view: WebView) -> Self {
        let presenter = Self {
            view,
            state: Rc::default(),
            rendered: Rc::default(),
            notify_pending: Rc::default(),
            timer: Rc::new(Timer::new(&view)),
        };
        // The native view owns these callbacks; destroying it also releases the timer.
        let tick = presenter.clone();
        presenter.timer.on_tick(move |_| tick.flush());
        view.add_script_message_handler("blindrssRenderBridge");
        let loaded = presenter.clone();
        view.on_loaded(move |event| {
            event.event.skip(true);
            if !event.get_target().unwrap_or_default().is_empty() {
                return;
            }
            loaded.state.borrow_mut().loaded();
            loaded.schedule();
        });
        let finished = presenter.clone();
        view.on_script_message_received(move |event| {
            event.event.skip(true);
            let Some(message) = event.get_string() else {
                return;
            };
            if let Some(generation) = message
                .strip_prefix("__rich_done:")
                .and_then(|s| s.parse().ok())
            {
                let completed = finished.state.borrow_mut().complete(generation);
                if completed {
                    finished.schedule();
                }
            } else if let Some(generation) = message
                .strip_prefix("__rich_error:")
                .and_then(|s| s.parse().ok())
            {
                finished.recover(generation);
            }
        });
        presenter
    }

    pub(crate) fn view(&self) -> WebView {
        self.view
    }

    pub(crate) fn on_rendered(&self, callback: impl Fn() + 'static) {
        *self.rendered.borrow_mut() = Some(Rc::new(callback));
    }

    fn notify_rendered(&self) {
        if !self.notify_pending.replace(false) {
            return;
        }
        let callback = self.rendered.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    /// Keep document and iframe starts inside the view until the latest swap completes.
    pub(crate) fn is_loading(&self) -> bool {
        self.state.borrow().applying()
    }

    /// Callers still provide the sanitizer's complete, unmodified article fragment.
    pub(crate) fn render(&self, fragment: &str, url: &str) {
        self.state.borrow_mut().request(fragment, url);
        self.notify_pending.set(true);
        self.schedule();
    }

    fn schedule(&self) {
        if !self.timer.is_running() {
            self.timer.start(1, true);
        }
    }

    fn flush(&self) {
        let navigation = {
            let mut state = self.state.borrow_mut();
            state.begin_navigation().map(|recovery| {
                let update = state.desired.as_ref().unwrap();
                (
                    if recovery {
                        update.html.clone()
                    } else {
                        String::new()
                    },
                    update.url.clone(),
                )
            })
        };
        if let Some((html, url)) = navigation {
            self.view.set_page(&document(&html, &url), &url);
            return;
        }
        let complete = {
            let state = self.state.borrow();
            state
                .desired
                .as_ref()
                .is_some_and(|u| u.generation == state.completed)
        };
        if complete {
            self.notify_rendered();
        } else {
            self.dispatch();
        }
    }

    fn dispatch(&self) {
        let update = self.state.borrow_mut().take_update();
        let Some(update) = update else { return };
        let script = queue_script(&update);
        // wxDragon's RunScript waits synchronously; the queued callback owns DOM work.
        let accepted = self.view.run_script(&script);
        if accepted.as_deref() != Some("true") {
            self.recover(update.generation);
        }
    }

    fn recover(&self, generation: u64) {
        if self.state.borrow_mut().recover(generation) {
            self.schedule();
        }
    }
}

impl Deref for RichView {
    type Target = WebView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

fn queue_script(update: &Update) -> String {
    let html = serde_json::to_string(&update.html).expect("an HTML string serializes");
    let url = serde_json::to_string(&update.url).expect("a URL string serializes");
    format!(
        "Boolean(window.__blindrssRich && window.__blindrssRich.queue({{generation:{},html:{html},url:{url}}}));",
        update.generation
    )
}

fn document(fragment: &str, url: &str) -> String {
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let base = escape(url);
    let language = blindrss_core::article_lang::normalize_lang(Some(&crate::ui_language()))
        .unwrap_or_else(|| blindrss_core::article_lang::DEFAULT_LANG.into());
    crate::rich_document(fragment)
        .replacen("<html>", &format!("<html lang=\"{language}\">"), 1)
        .replacen(
            "<head>",
            &format!("<head><base id=\"blindrss-base\" href=\"{base}\">"),
            1,
        )
        .replacen(
            "</body>",
            &format!("<script>{RUNTIME_JS}</script></body>"),
            1,
        )
}

const RUNTIME_JS: &str = r#"
(function(){
  var content=document.getElementById('content');
  var base=document.getElementById('blindrss-base');
  var latest=0, pending=null, timer=null;
  function post(kind,generation){
    if(window.blindrssRenderBridge) window.blindrssRenderBridge.postMessage(kind+generation);
  }
  window.__blindrssRich={queue:function(update){
    if(update.generation<=latest) return true;
    latest=update.generation;
    pending=update;
    if(timer!==null) return true;
    timer=setTimeout(function(){
      timer=null;
      var next=pending; pending=null;
      if(!next || next.generation!==latest) return;
      var focused=document.hasFocus() && (document.activeElement===document.body || content.contains(document.activeElement));
      content.setAttribute('aria-busy','true');
      var ok=false;
      try{
        var x=window.scrollX, y=window.scrollY, previous=base.href;
        base.href=next.url;
        var changedUrl=previous!==base.href;
        var template=document.createElement('template');
        template.innerHTML=next.html;
        if(next.generation!==latest) return;
        content.replaceChildren(template.content);
        window.scrollTo(changedUrl?0:x,changedUrl?0:y);
        if(focused) content.focus({preventScroll:true});
        ok=true;
      }finally{
        content.setAttribute('aria-busy','false');
        post(ok?'__rich_done:':'__rich_error:',next.generation);
      }
    },0);
    return true;
  }};
})();
"#;

#[derive(Clone, Debug)]
struct Update {
    generation: u64,
    html: String,
    url: String,
}

#[derive(Default)]
enum DocumentState {
    #[default]
    Empty,
    Scheduled(bool),
    Loading,
    Ready,
}

#[derive(Default)]
struct RenderState {
    document: DocumentState,
    desired: Option<Update>,
    queued: u64,
    completed: u64,
}

impl RenderState {
    fn request(&mut self, html: &str, url: &str) -> bool {
        if self
            .desired
            .as_ref()
            .is_some_and(|u| u.html == html && u.url == url)
        {
            return false;
        }
        let generation = self.desired.as_ref().map_or(1, |u| u.generation + 1);
        self.desired = Some(Update {
            generation,
            html: html.into(),
            url: url.into(),
        });
        if matches!(self.document, DocumentState::Empty) {
            self.document = DocumentState::Scheduled(false);
        }
        true
    }

    fn loading(&self) -> bool {
        matches!(
            self.document,
            DocumentState::Scheduled(_) | DocumentState::Loading
        )
    }

    fn applying(&self) -> bool {
        self.loading()
            || self
                .desired
                .as_ref()
                .is_some_and(|u| u.generation != self.completed)
    }

    fn begin_navigation(&mut self) -> Option<bool> {
        let DocumentState::Scheduled(recovery) = self.document else {
            return None;
        };
        self.document = DocumentState::Loading;
        if recovery {
            self.queued = self.desired.as_ref()?.generation;
        }
        Some(recovery)
    }

    fn loaded(&mut self) -> bool {
        if matches!(self.document, DocumentState::Loading) {
            self.document = DocumentState::Ready;
            // Recovery loads the article itself, so it completes at document load.
            if self.queued != 0
                && self
                    .desired
                    .as_ref()
                    .is_some_and(|u| u.generation == self.queued)
            {
                return self.complete(self.queued);
            }
        }
        false
    }

    fn take_update(&mut self) -> Option<Update> {
        if !matches!(self.document, DocumentState::Ready) {
            return None;
        }
        let update = self.desired.as_ref()?;
        if update.generation == self.queued {
            return None;
        }
        self.queued = update.generation;
        Some(update.clone())
    }

    fn complete(&mut self, generation: u64) -> bool {
        if self.completed == generation
            || self
                .desired
                .as_ref()
                .is_none_or(|u| u.generation != generation)
        {
            return false;
        }
        self.completed = generation;
        true
    }

    fn recover(&mut self, generation: u64) -> bool {
        let Some(update) = self.desired.as_ref() else {
            return false;
        };
        if update.generation != generation || self.completed == generation || self.loading() {
            return false;
        }
        // A failed script falls back to full-page rendering, on the next UI turn.
        self.document = DocumentState::Scheduled(true);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_retains_only_the_latest_article() {
        let mut state = RenderState::default();
        assert!(state.request("first", "https://one.test/"));
        assert!(state.loading());
        assert!(state.take_update().is_none());
        assert!(
            !state.loaded(),
            "bootstrap load cannot complete the scheduled shell"
        );
        assert!(state.take_update().is_none());
        assert!(state.request("last", "https://two.test/"));
        state.begin_navigation();
        state.loaded();
        let update = state.take_update().unwrap();
        assert_eq!(
            (update.html.as_str(), update.url.as_str()),
            ("last", "https://two.test/")
        );
        assert!(state.take_update().is_none());
    }

    #[test]
    fn duplicates_do_not_replace_the_accessibility_tree() {
        let mut state = RenderState::default();
        state.request("article", "https://one.test/");
        state.begin_navigation();
        state.loaded();
        let generation = state.take_update().unwrap().generation;
        assert!(
            state.applying(),
            "the shell is ready but the DOM is not applied yet"
        );
        assert!(!state.request("article", "https://one.test/"));
        assert!(state.take_update().is_none());
        assert!(state.complete(generation));
        assert!(!state.applying());
        assert!(
            !state.complete(generation),
            "duplicate acknowledgements do not notify twice"
        );
        assert!(!state.request("article", "https://one.test/"));
        assert!(state.request("article", "https://two.test/"));
        assert_ne!(state.take_update().unwrap().generation, generation);
    }

    #[test]
    fn stale_completion_and_failure_do_not_replace_the_current_article() {
        let mut state = RenderState::default();
        state.request("first", "https://one.test/");
        state.begin_navigation();
        state.loaded();
        let old = state.take_update().unwrap().generation;
        state.request("last", "https://two.test/");
        let latest = state.take_update().unwrap().generation;
        assert!(!state.complete(old));
        assert!(!state.recover(old));
        assert!(!state.loading());
        assert!(state.recover(latest));
        assert_eq!(state.desired.as_ref().unwrap().html, "last");
        assert!(state.loading());
        state.begin_navigation();
        assert!(state.loaded());
        assert!(
            state.take_update().is_none(),
            "recovery already renders the full article"
        );
    }

    #[test]
    fn a_new_article_replaces_a_scheduled_recovery_without_a_second_swap() {
        let mut state = RenderState::default();
        state.request("first", "https://one.test/");
        state.begin_navigation();
        state.loaded();
        let failed = state.take_update().unwrap().generation;
        assert!(state.recover(failed));
        state.request("latest", "https://two.test/");
        assert_eq!(state.begin_navigation(), Some(true));
        assert_eq!(state.desired.as_ref().unwrap().html, "latest");
        assert!(state.loaded());
        assert!(!state.applying());
        assert!(state.take_update().is_none());
    }
}

#[cfg(test)]
mod script_tests {
    use super::*;

    #[test]
    fn queue_keeps_the_entire_fragment_out_of_the_synchronous_dom_path() {
        let html = format!(
            "<article lang=\"fr\"><h1>Title</h1><p>{}</p><p>UNIQUE_TAIL</p></article>",
            "<>&\"\\\n".repeat(20_000)
        );
        let update = Update {
            generation: 7,
            html,
            url: "https://example.test/new/?a=1&b=2".into(),
        };
        let script = queue_script(&update);
        assert!(script.contains(&serde_json::to_string(&update.html).unwrap()));
        assert!(script.contains("generation:7"));
        assert!(!script.contains("replaceChildren"));
        assert!(RUNTIME_JS.contains("setTimeout(function()"));
        assert!(RUNTIME_JS.contains("next.generation!==latest"));
        assert!(RUNTIME_JS.contains("content.replaceChildren(template.content)"));
        assert!(
            RUNTIME_JS.find("base.href=next.url").unwrap()
                < RUNTIME_JS.find("template.innerHTML=next.html").unwrap()
        );
        assert!(RUNTIME_JS.contains("content.setAttribute('aria-busy','true')"));
        assert!(RUNTIME_JS.contains("finally{"));
        assert!(RUNTIME_JS.contains("content.setAttribute('aria-busy','false')"));
        assert!(RUNTIME_JS.contains("if(focused) content.focus({preventScroll:true})"));
    }

    #[test]
    fn shell_preserves_article_language_and_escapes_the_base_attribute() {
        let fragment = "<article lang=\"fr\"><h1>Title</h1><a href=\"relative\">Link</a></article>";
        let shell = document(fragment, "https://example.test/?a=1&b=\"two\"");
        assert!(shell.contains("<html lang=\""));
        assert!(shell.contains("href=\"https://example.test/?a=1&amp;b=&quot;two&quot;\""));
        assert!(shell.contains("id=\"content\" tabindex=\"-1\""));
        assert!(shell.contains(fragment));
        assert!(!shell.contains("aria-live"));
    }
}

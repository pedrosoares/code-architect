//! Drives a real, headless Firefox (through `geckodriver`) and exercises all
//! seven browser tools end-to-end — open a page, fill a field, click, read the
//! console the page logged, read the resulting DOM state, screenshot, and
//! close. This is the proof that the "the LLM can open pages, read logs, and
//! interact" loop actually works against a live browser, not just that the
//! tool names and schemas are right.
//!
//! The real loop is `#[ignore]`d by default because it spawns `geckodriver`
//! *and* `firefox`; both must be installed (or `FIREFOX_GECKODRIVER` set).
//! Run it with:
//!
//! ```sh
//! cargo test -p architect-firefox -- --ignored --nocapture
//! ```
//!
//! The non-ignored test needs neither — it only touches the API surface.

use std::sync::Arc;

use architect_firefox::{Browser, BrowserEvent};
use base64::Engine;

/// The page the loop drives. Deliberately no `console.log` on load — the
/// interesting log comes from the *interaction* (`#go`'s click handler),
/// which is exactly the case the console-hook design must handle: the hook
/// must already be installed (from `firefox_open`) before that click fires.
const PAGE: &str = r#"<!doctype html>
<html><head><title>ca-e2e</title></head><body>
  <input id="q">
  <select id="pick"><option value="one">One</option><option value="two">Two</option></select>
  <button id="go">Go</button>
  <span id="out"></span>
  <script>
    document.getElementById("go").addEventListener("click", function () {
      var v = document.getElementById("q").value;
      console.log("clicked:" + v);
      document.getElementById("out").textContent = "did:" + v;
    });
  </script>
</body></html>"#;

/// A `Browser` that has never opened still behaves sanely: `status` reports
/// nothing running, and `close` is a no-op (idempotent) rather than an error.
/// No driver, no Firefox required — this runs everywhere.
#[tokio::test]
async fn a_fresh_browser_is_closed_safely_and_reports_not_running() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BrowserEvent>();
    let browser = Browser::new(tx);

    let status = browser.status().await;
    assert!(!status.running, "a fresh browser has no geckodriver");
    assert!(!status.has_session, "a fresh browser has no session");

    // `close` must be a no-op, not an error, when nothing was ever started —
    // this is the exact path the engine takes at shutdown for a session that
    // never touched a web page.
    browser.close().await;
    browser.close().await;

    // It emitted a `Closed` event (the UI folds this into its Browser summary).
    let mut saw_closed = false;
    while let Ok(event) = rx.try_recv() {
        if event == BrowserEvent::Closed {
            saw_closed = true;
        }
    }
    assert!(saw_closed, "close() should report a Closed event");
}

/// The full loop against a live headless Firefox: `firefox_open` a local page,
/// `firefox_fill` a field, `firefox_click` a button whose handler logs, read
/// that log via `firefox_logs`, confirm the click's DOM side-effect via
/// `firefox_eval`, capture a real PNG via `firefox_screenshot`, and tear it
/// all down with `firefox_close`.
#[tokio::test]
#[ignore = "spawns real geckodriver + headless Firefox (both must be installed)"]
async fn drives_a_real_headless_firefox_end_to_end() {
    // Drive a local page via a file:// URL — no network, fully deterministic.
    let dir = tempfile::tempdir().expect("tempdir");
    let page = dir.path().join("page.html");
    std::fs::write(&page, PAGE).expect("write the test page");
    let url = format!("file://{}", page.to_string_lossy());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BrowserEvent>();
    let browser = Arc::new(Browser::new(tx));
    let all: Vec<Arc<dyn architect_tools::Tool>> = architect_firefox::tools(browser.clone());
    let tool = |name: &str| {
        all.iter()
            .find(|t| t.name() == name)
            .cloned()
            .unwrap_or_else(|| panic!("no tool named {name}"))
    };

    // `ToolContext` is unused by the browser tools, so any existing root works.
    let ctx = architect_tools::ToolContext::new(dir.path()).expect("tool context");

    // 1. open — the title proves a real Firefox actually loaded the page.
    let out = tool("firefox_open")
        .call(serde_json::json!({ "url": url }), &ctx)
        .await
        .expect("firefox_open");
    assert!(
        out.text.contains("ca-e2e"),
        "title should be ca-e2e, got: {}",
        out.text
    );

    // 2. fill a field (fires the page's own input events).
    tool("firefox_fill")
        .call(
            serde_json::json!({ "selector": "#q", "value": "hello" }),
            &ctx,
        )
        .await
        .expect("firefox_fill");

    // 2b. fill a `<select>` by option value — the old clear-then-type path
    //     failed with "Unable to clear element that cannot be edited"; the
    //     select must now be settable (matched by option value, then label,
    //     with the page's change event fired).
    tool("firefox_fill")
        .call(
            serde_json::json!({ "selector": "#pick", "value": "two" }),
            &ctx,
        )
        .await
        .expect("firefox_fill on a select");
    let out = tool("firefox_eval")
        .call(
            serde_json::json!({ "script": "document.getElementById('pick').value" }),
            &ctx,
        )
        .await
        .expect("eval the select value");
    assert!(
        out.text.contains("two"),
        "the select should be set to 'two', got: {}",
        out.text
    );

    // 3. click a button whose handler does `console.log("clicked:hello")` and
    //    writes to the DOM.
    tool("firefox_click")
        .call(serde_json::json!({ "selector": "#go" }), &ctx)
        .await
        .expect("firefox_click");

    // 4. read the console — the click's log must already be captured, i.e. the
    //    hook was in place since `firefox_open`, not just installed now.
    let out = tool("firefox_logs")
        .call(serde_json::json!({ "clear": true }), &ctx)
        .await
        .expect("firefox_logs");
    assert!(
        out.text.contains("clicked:hello"),
        "the interaction's console.log should be captured, got: {}",
        out.text
    );

    // 5. read the DOM state the click produced — proves the click really ran.
    let out = tool("firefox_eval")
        .call(
            serde_json::json!({ "script": "document.getElementById('out').textContent" }),
            &ctx,
        )
        .await
        .expect("firefox_eval");
    assert!(
        out.text.contains("did:hello"),
        "eval should see did:hello, got: {}",
        out.text
    );

    // 5b. an IIFE and a multi-line statement script must both return their
    //     value (the original "IIFE returns null" and "multi-line
    //     SyntaxError" bugs) — the bare expression, an IIFE, and a
    //     let-statement that sets + returns all come back correctly.
    let out = tool("firefox_eval")
        .call(serde_json::json!({ "script": "(() => 42)()" }), &ctx)
        .await
        .expect("IIFE eval");
    assert_eq!(
        out.text.trim(),
        "42",
        "an IIFE should return 42, got: {}",
        out.text
    );
    let out = tool("firefox_eval")
        .call(
            serde_json::json!({ "script": "let n = 5; return n * 2;" }),
            &ctx,
        )
        .await
        .expect("multi-statement eval");
    assert_eq!(
        out.text.trim(),
        "10",
        "a multi-statement eval should return 10, got: {}",
        out.text
    );

    // 6. screenshot — the PNG is attached inline (so the chat renders it and
    //    the model sees it directly) AND saved to the workspace for `view_image`
    //    to re-open later. Verify both: the inline image decodes to a genuine
    //    PNG, and the saved file on disk is a genuine PNG.
    let out = tool("firefox_screenshot")
        .call(serde_json::json!({}), &ctx)
        .await
        .expect("firefox_screenshot");

    // The inline image — the new behavior that makes the chat render it.
    let image = out
        .image
        .as_ref()
        .expect("firefox_screenshot should now return the image inline");
    assert_eq!(image.media_type, "image/png");
    let inline_png = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .expect("the inline image should be valid base64");
    assert!(
        inline_png.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
        "inline screenshot should begin with the PNG magic bytes, got {:02x?}",
        &inline_png[..4.min(inline_png.len())]
    );

    // The saved file — kept so `view_image` can re-open it and the path stays
    // reportable.
    assert!(
        out.text.contains(".coder/screenshots"),
        "should report the saved path, got: {}",
        out.text
    );
    let saved = ctx.workspace_root().join(
        out.text
            .split_whitespace()
            .find(|tok| tok.ends_with(".png"))
            .expect("a .png path in the screenshot output"),
    );
    let png = std::fs::read(&saved).expect("the screenshot file should exist");
    assert!(
        png.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
        "screenshot should begin with the PNG magic bytes, got {:02x?}",
        &png[..4.min(png.len())]
    );

    // 7. close — tears down the session and geckodriver; idempotent.
    tool("firefox_close")
        .call(serde_json::json!({}), &ctx)
        .await
        .expect("firefox_close");

    // The lifecycle must have flowed through the engine-facing channel.
    let mut kinds: Vec<&str> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        kinds.push(event.kind());
    }
    for expected in ["driver_started", "session_started", "navigated", "closed"] {
        assert!(
            kinds.contains(&expected),
            "expected a {expected:?} lifecycle event, saw: {kinds:?}"
        );
    }
}

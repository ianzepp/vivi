use axum::http::StatusCode;

use super::super::browser_test::Browser;
use crate::data::mail::tests::seed_mail;

#[tokio::test]
async fn the_mail_list_shows_threads_and_filter_chips() {
    let (store, _storage) = seed_mail();
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/mail").await;
    assert_eq!(status, StatusCode::OK);
    for expected in [
        "Other",
        "Root",
        "identity=mind",
        "role=needs",
        "kind=need",
        "/ui/p/demo/mail/",
    ] {
        assert!(page.contains(expected), "{expected} missing from {page}");
    }
    assert!(!page.contains("Re: Root"), "a reply is not a thread root");
    assert!(!page.contains("not built yet"));
}

#[tokio::test]
async fn the_list_filters_from_the_query_string() {
    let (store, _storage) = seed_mail();
    let browser = Browser::open(store.catalog()).await;
    let (_, needs) = browser.get("/ui/p/demo/mail?kind=need").await;
    assert!(
        needs.contains("Root") && !needs.contains(">Other<"),
        "{needs}"
    );
    let (_, none) = browser.get("/ui/p/demo/mail?kind=task").await;
    assert!(none.contains("No threads match."));
}

#[tokio::test]
async fn a_thread_shows_every_letter_in_order_with_escaped_bodies() {
    let (store, _storage) = seed_mail();
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/mail/aaaa1111").await;
    assert_eq!(status, StatusCode::OK);
    let root = page.find("Please fix").expect("root body");
    let reply = page.find("Done, see commit.").expect("reply body");
    assert!(root < reply, "oldest first");
    assert!(page.contains("<pre class=\"mailbody\">"));
    assert!(!page.contains("<b>this</b>"), "bodies are escaped: {page}");
    assert!(page.contains("Please fix &lt;b&gt;this&lt;/b&gt; &amp; that."));
    // Markdown is shown as typed, not rendered.
    assert!(page.contains("- [ ] one"));
    assert!(page.contains("Answers: "));
    assert!(page.contains("2 letters, oldest first."));
}

#[tokio::test]
async fn an_unknown_handle_is_a_404_and_odd_text_stays_text() {
    let (store, _storage) = seed_mail();
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/mail/zzzz9999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(page.contains("No letter matches"));
    let (status, page) = browser.get("/ui/p/demo/mail/%27%3Cscript%3E").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!page.contains("<script>"), "{page}");
}

#[tokio::test]
async fn a_project_with_no_store_says_so() {
    let (dir, root) = crate::test_support::project_without_store();
    let catalog = crate::host::Catalog {
        projects: vec![crate::host::ProjectRef {
            id: "proj-1".into(),
            name: "demo".into(),
            mailspace_root: root,
        }],
    };
    let browser = Browser::open(catalog).await;
    let (status, page) = browser.get("/ui/p/demo/mail").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("no mailspace store yet"));
    drop(dir);
}

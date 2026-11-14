use super::*;

#[test]
fn escape_covers_markup_characters_and_quotes() {
    assert_eq!(
        escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
        "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
    );
}

#[test]
fn text_is_escaped_and_raw_is_not() {
    assert_eq!(Html::text("<b>").as_str(), "&lt;b&gt;");
    assert_eq!(Html::raw("<b>").as_str(), "<b>");
}

#[test]
fn attribute_values_are_escaped() {
    let link = link_text("/x?a=1&b=\"2\"", "go <now>");
    assert_eq!(
        link.as_str(),
        "<a href=\"/x?a=1&amp;b=&quot;2&quot;\">go &lt;now&gt;</a>"
    );
}

#[test]
fn void_tags_have_no_closing_tag() {
    let input = void("input", &[("name", "key"), ("type", "password")]);
    assert_eq!(input.as_str(), "<input name=\"key\" type=\"password\">");
}

#[test]
fn join_concatenates_in_order() {
    let all = join([Html::text("a"), Html::text("<"), span("c", "b")]);
    assert_eq!(all.as_str(), "a&lt;<span class=\"c\">b</span>");
}

#[test]
fn url_encode_keeps_unreserved_and_encodes_the_rest() {
    assert_eq!(url_encode("slot-1_a.b~c"), "slot-1_a.b~c");
    assert_eq!(url_encode("a b/c?d"), "a%20b%2Fc%3Fd");
    assert_eq!(url_encode("é"), "%C3%A9");
}

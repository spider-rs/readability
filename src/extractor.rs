use super::dom;
use super::error::Error;
use super::scorer;
use crate::rcdom::{RcDom, SerializableHandle};
use html5ever::tendril::stream::TendrilSink;
use html5ever::{parse_document, serialize};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::default::Default;
use std::io::Read;
use std::path::Path;
use url::Url;

#[derive(Debug)]
pub struct Product {
    /// The HTML content.
    pub content: String,
    /// The text content raw.
    pub text: String,
}

/// Readability alg extract a website url.
pub fn extract<R>(input: &mut R, url: &Url) -> Result<Product, Error>
where
    R: Read,
{
    let mut dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(input)?;

    let mut candidates = BTreeMap::new();
    let mut nodes = BTreeMap::new();
    let mut id: &str = "/";
    let mut bytes = vec![];
    let mut text: String = String::new();
    let mut title: String = String::new();
    let mut lang: String = String::new();

    let handle = dom.document.clone();

    scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);
    scorer::find_candidates(Path::new(id), &handle, &mut candidates, &mut nodes);

    let mut top_candidate: &scorer::Candidate = &scorer::Candidate {
        node: handle,
        score: Cell::new(0.0),
    };

    for (i, c) in candidates.iter() {
        let score = c.score.get() * (1.0 - scorer::get_link_density(&c.node));
        c.score.set(score);
        if score <= top_candidate.score.get() {
            continue;
        }
        id = i;
        top_candidate = c;
    }

    let node = &top_candidate.node;

    scorer::clean(&mut dom, Path::new(id), &node, url, &candidates);

    serialize(
        &mut bytes,
        &SerializableHandle::from(node.clone()),
        Default::default(),
    )
    .ok();

    let content = auto_encoder::auto_encode_bytes(bytes.as_slice());

    dom::extract_text(&node, &mut text, true);

    let html_content = format!(
        r#"<html class="paper"{}><head>
<meta name="disabled-adaptations" content="watch">
<meta http-equiv="Content-Type" content="text/html; charset=utf-8">
<meta name="viewport" content="initial-scale=1">
<base href="{url}">
{}
<script>window.isReaderPage = true;</script>
</head><body>
"#,
        if !lang.is_empty() {
            format!(r#" lang="{}""#, &lang)
        } else {
            "".into()
        },
        if title.is_empty() {
            "".into()
        } else {
            format!("<title>{title}</title>")
        }
    );

    let formatted_content = format!("{}{}</body></html>", html_content, content);

    Ok(Product {
        content: formatted_content,
        text,
    })
}

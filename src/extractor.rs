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

#[cfg(feature = "tokio")]
use html5ever::tendril::stream::Utf8LossyDecoder;
#[cfg(feature = "tokio")]
use html5ever::tendril::{fmt as tendril_fmt, Tendril};
#[cfg(feature = "tokio")]
use html5ever::Parser;
#[cfg(feature = "tokio")]
use tokio::io::{AsyncRead, AsyncReadExt};

/// Above this size the async variants chunk input into html5ever's sink and
/// yield cooperatively between chunks, so the executor isn't starved by a
/// long synchronous parse. Below it, parsing is fast enough that yielding
/// would cost more than it saves.
#[cfg(feature = "tokio")]
pub const ASYNC_BYTE_THRESHOLD: usize = 128 * 1024;

/// Read / feed buffer size used when streaming bytes into html5ever's sink.
#[cfg(feature = "tokio")]
const STREAM_READ_CHUNK: usize = 32 * 1024;

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
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(input)?;
    process_dom(dom, url)
}

/// Run the readability pipeline on an already-parsed DOM. Used by both the
/// sync entry point and the async streaming variant after the sink finishes.
pub fn process_dom(mut dom: RcDom, url: &Url) -> Result<Product, Error> {
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

/// Async variant of [`extract`] for callers that already have the body buffered.
///
/// Below [`ASYNC_BYTE_THRESHOLD`] the body is small enough that a single
/// straight-through parse won't meaningfully starve the executor, so we feed
/// it to the sink in one shot. Above the threshold we feed chunks into
/// html5ever's [`TendrilSink`] and call [`tokio::task::yield_now`] between
/// each chunk so the executor stays responsive.
///
/// The returned future is `!Send` because [`RcDom`] uses `Rc<RefCell<…>>`.
/// Run it on a `current_thread` runtime, in a [`tokio::task::LocalSet`],
/// or via [`tokio::task::spawn_local`].
#[cfg(feature = "tokio")]
pub async fn extract_async(bytes: Vec<u8>, url: Url) -> Result<Product, Error> {
    let mut sink: Utf8LossyDecoder<Parser<RcDom>> =
        parse_document(RcDom::default(), Default::default()).from_utf8();

    if bytes.len() < ASYNC_BYTE_THRESHOLD {
        // Small enough to feed in one shot — yields would cost more than they save.
        sink.process(Tendril::<tendril_fmt::Bytes>::from_slice(&bytes));
        let dom = sink.finish();
        return process_dom(dom, &url);
    }

    // Large: chunk through the sink and yield between chunks so we don't
    // monopolize the executor for a multi-millisecond CPU burst.
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + STREAM_READ_CHUNK).min(bytes.len());
        sink.process(Tendril::<tendril_fmt::Bytes>::from_slice(
            &bytes[offset..end],
        ));
        offset = end;
        tokio::task::yield_now().await;
    }
    let dom = sink.finish();
    process_dom(dom, &url)
}

/// Streaming async variant: pulls bytes from `reader` and feeds them straight
/// into html5ever's [`TendrilSink`] on the current task, chunk by chunk. The
/// `await` on each `reader.read()` is the cooperative yield point — there is
/// no thread hop, no `spawn_blocking`, no channel.
///
/// The returned future is `!Send` because the parser/[`RcDom`] use
/// `Rc<RefCell<…>>`. Run it on a `current_thread` runtime, in a
/// [`tokio::task::LocalSet`], or via [`tokio::task::spawn_local`].
#[cfg(feature = "tokio")]
pub async fn extract_async_reader<R>(mut reader: R, url: Url) -> Result<Product, Error>
where
    R: AsyncRead + Unpin,
{
    let mut sink: Utf8LossyDecoder<Parser<RcDom>> =
        parse_document(RcDom::default(), Default::default()).from_utf8();
    let mut buf = vec![0u8; STREAM_READ_CHUNK];
    let mut since_yield = 0usize;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        sink.process(Tendril::<tendril_fmt::Bytes>::from_slice(&buf[..n]));
        since_yield += n;
        // If the underlying reader keeps returning Ready (e.g. an in-memory
        // AsyncRead), insert an explicit yield once we've accumulated more
        // than the threshold so we don't starve the executor.
        if since_yield >= ASYNC_BYTE_THRESHOLD {
            tokio::task::yield_now().await;
            since_yield = 0;
        }
    }
    let dom = sink.finish();
    process_dom(dom, &url)
}

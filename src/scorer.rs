use super::dom;
use crate::rcdom::Handle;
use crate::rcdom::Node;
use crate::rcdom::NodeData::{Comment, Doctype, Document, Element, ProcessingInstruction, Text};
use crate::rcdom::RcDom;
use html5ever::ns;
use html5ever::tree_builder::TreeSink;
use html5ever::tree_builder::{ElementFlags, NodeOrText};
use html5ever::{LocalName, QualName};
use regex::Regex;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;
use url::Url;

pub static PUNCTUATIONS_REGEX: &str = r"([、。，．！？]|\.[^A-Za-z0-9]|,[^0-9]|!|\?)";
pub static UNLIKELY_CANDIDATES: &str = "combx|comment|community|disqus|extra|foot|header|menu\
     |remark|rss|shoutbox|sidebar|sponsor|ad-break|agegate\
     |pagination|pager|popup|tweet|twitter\
     |ssba";
pub static LIKELY_CANDIDATES: &str = "and|article|body|column|main|shadow\
                                              |content|hentry";
pub static POSITIVE_CANDIDATES: &str = "article|body|content|entry|hentry|main|page\
     |pagination|post|text|blog|story";
pub static NEGATIVE_CANDIDATES: &str = "combx|comment|com|contact|foot|footer|footnote\
     |masthead|media|meta|outbrain|promo|related\
     |scroll|shoutbox|sidebar|sponsor|shopping\
     |tags|tool|widget|form|textfield\
     |uiScale|hidden";
static BLOCK_CHILD_TAGS: &[&str] = &[
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "dt",
    "dd",
    "figure",
    "footer",
    "header",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "nav",
    "p",
    "pre",
    "section",
    "table",
    "ul",
    "ol",
    "li",
];

static PUNCTUATIONS: LazyLock<Regex> = LazyLock::new(|| Regex::new(PUNCTUATIONS_REGEX).unwrap());
static LIKELY: LazyLock<Regex> = LazyLock::new(|| Regex::new(LIKELY_CANDIDATES).unwrap());
static UNLIKELY: LazyLock<Regex> = LazyLock::new(|| Regex::new(UNLIKELY_CANDIDATES).unwrap());
static POSITIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(POSITIVE_CANDIDATES).unwrap());
static NEGATIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(NEGATIVE_CANDIDATES).unwrap());

#[derive(Debug, Clone)]
pub struct Candidate {
    /// The node of the candidate.
    pub node: Rc<Node>,
    /// The score.
    pub score: Cell<f32>,
}

/// valid web asset
pub fn local_url(s: &str) -> bool {
    !s.starts_with("//") && !s.starts_with("http://") && !s.starts_with("https://")
}

/// Fix an image path.
pub fn fix_img_path(handle: &Handle, url: &Url) -> bool {
    let src = dom::get_attr("src", &handle);
    let s = match src {
        Some(src) => src,
        None => return false,
    };
    if local_url(&s) {
        if let Ok(new_url) = url.join(&s) {
            dom::set_attr("src", new_url.as_str(), &handle)
        }
    }
    true
}

pub fn fix_anchor_path(handle: &Handle, url: &Url) -> bool {
    let src = dom::get_attr("href", &handle);
    let s = match src {
        Some(src) => src,
        None => return false,
    };
    if local_url(&s) {
        if let Ok(new_url) = url.join(&s) {
            dom::set_attr("href", new_url.as_str(), &handle)
        }
    }
    true
}

pub fn get_link_density(handle: &Handle) -> f32 {
    let text_length = dom::text_len(&handle) as f32;
    if text_length == 0.0 {
        return 0.0;
    }
    let mut link_length = 0.0;
    let mut links: Vec<Rc<Node>> = vec![];

    dom::find_node(&handle, "a", &mut links);
    for link in links.iter() {
        link_length += dom::text_len(&link) as f32;
    }
    link_length / text_length
}

/// determine if the element is a valid candiate for reading.
pub fn is_candidate(handle: &Handle) -> bool {
    let text_len = dom::text_len(&handle);
    if text_len < 4 {
        return false;
    }
    let n: &str = &dom::get_tag_name(&handle).unwrap_or_default();

    match n {
        "p" | "h1" | "h2" => true,
        "div" | "article" | "center" | "section" | "header" => {
            dom::has_nodes(handle, BLOCK_CHILD_TAGS)
        }
        _ => false,
    }
}

pub fn init_content_score(handle: &Handle) -> f32 {
    let tag_name = dom::get_tag_name(&handle).unwrap_or_default();
    let score = match tag_name.as_ref() {
        "article" | "h1" | "header" => 10.0,
        "div" => 5.0,
        "blockquote" => 3.0,
        "form" => -3.0,
        "th" => 5.0,
        _ => 0.0,
    };
    score + get_class_weight(&handle)
}

pub fn calc_content_score(handle: &Handle) -> f32 {
    let mut score: f32 = 1.0;
    let mut text = String::new();

    dom::extract_text(&handle, &mut text, true);

    let mat = PUNCTUATIONS.find_iter(&text);
    score += mat.count() as f32;
    score += f32::min(f32::floor(text.chars().count() as f32 / 100.0), 3.0);
    score
}

pub fn get_class_weight(handle: &Handle) -> f32 {
    let mut weight: f32 = 0.0;
    if let Element {
        name: _, ref attrs, ..
    } = handle.data
    {
        for name in ["id", "class"].iter() {
            if let Some(val) = dom::attr(name, &attrs.borrow()) {
                if POSITIVE.is_match(&val) {
                    weight += 25.0
                };
                if NEGATIVE.is_match(&val) {
                    weight -= 25.0
                }
            }
        }
    };
    weight
}

/// Pre-process handle dom adjusting.
pub fn preprocess(dom: &mut RcDom, handle: &Handle, title: &mut String, lang: &mut String) -> bool {
    if let Element {
        ref name,
        ref attrs,
        ..
    } = handle.data
    {
        let tag_name = name.local.as_ref();

        match tag_name.to_lowercase().as_ref() {
            "script" | "link" | "style" => return true,
            "title" => {
                if title.is_empty() {
                    dom::extract_text(&handle, title, true);
                }
            }
            "html" => {
                if let Some(val) = dom::attr("lang", &attrs.borrow()) {
                    if lang.is_empty() {
                        *lang = val;
                    }
                }
            }
            _ => (),
        }

        for name in ["id", "class"].iter() {
            if let Some(val) = dom::attr(name, &attrs.borrow()) {
                if tag_name != "body" && UNLIKELY.is_match(&val) && !LIKELY.is_match(&val) {
                    return true;
                }
            }
        }
    }

    let mut useless_nodes = vec![];
    let mut paragraph_nodes = vec![];
    let mut br_count = 0;

    for child in handle.children.borrow().iter() {
        if preprocess(dom, &child, title, lang) {
            useless_nodes.push(child.clone());
        }

        match &child.data {
            Element { ref name, .. } => {
                let tag_name = name.local.as_ref();
                if "br" == tag_name.to_lowercase() {
                    br_count += 1
                } else {
                    br_count = 0
                }
            }
            Text { ref contents } => {
                let s = contents.borrow();
                if br_count >= 2 && !s.trim().is_empty() {
                    paragraph_nodes.push(child.clone());
                    br_count = 0
                }
            }
            _ => (),
        }
    }

    for node in useless_nodes.iter() {
        dom.remove_from_parent(node);
    }

    for node in paragraph_nodes.iter() {
        let name = QualName::new(None, ns!(), LocalName::from("p"));
        let p = dom.create_element(name, vec![], ElementFlags::default());
        dom.append_before_sibling(node, NodeOrText::AppendNode(p.clone()));
        dom.remove_from_parent(node);

        if let Text { ref contents } = &node.data {
            let text = contents.take();
            dom.append(&p, NodeOrText::AppendText(text))
        }
    }
    false
}

pub fn find_candidates(
    id: &Path,
    handle: &Handle,
    candidates: &mut BTreeMap<String, Candidate>,
    nodes: &mut BTreeMap<String, Rc<Node>>,
) {
    if let Some(id_str) = id.to_str().map(|id| id.to_string()) {
        nodes.insert(id_str, handle.clone());
    }

    if is_candidate(handle) {
        let score = calc_content_score(handle);

        // Add score to parent candidate
        if let Some(c) = id
            .parent()
            .and_then(|pid| find_or_create_candidate(pid, candidates, nodes))
        {
            c.score.set(c.score.get() + score)
        }

        // Add half score to grandparent candidate
        if let Some(c) = id
            .parent()
            .and_then(|pid| pid.parent())
            .and_then(|gpid| find_or_create_candidate(gpid, candidates, nodes))
        {
            c.score.set(c.score.get() + score / 2.0)
        }

        // Add score to self if already a candidate
        if let Some(c) = id
            .to_str()
            .and_then(|id_str| candidates.get(id_str))
        {
            c.score.set(c.score.get() + score)
        }

        // Add score to parent if already a candidate
        if let Some(c) = id
            .parent()
            .and_then(|pid| pid.to_str())
            .and_then(|pid_str| candidates.get(pid_str))
        {
            c.score.set(c.score.get() + score)
        }

        // Add score to grandparent if already a candidate
        if let Some(c) = id
            .parent()
            .and_then(|p| p.parent())
            .and_then(|gpid| gpid.to_str())
            .and_then(|gpid_str| candidates.get(gpid_str))
        {
            c.score.set(c.score.get() + score)
        }
    }

    for (i, child) in handle.children.borrow().iter().enumerate() {
        find_candidates(id.join(i.to_string()).as_path(), &child, candidates, nodes)
    }
}

/// Find or create a valid canditate.
fn find_or_create_candidate<'a>(
    id: &Path,
    candidates: &'a mut BTreeMap<String, Candidate>,
    nodes: &BTreeMap<String, Rc<Node>>,
) -> Option<&'a Candidate> {
    if let Some(id_str) = id.to_str() {
        if let Some(node) = nodes.get(id_str) {
            let candidate = candidates
                .entry(id_str.to_string())
                .or_insert_with(|| Candidate {
                    score: Cell::new(init_content_score(node)),
                    node: node.clone(),
                });

            return Some(candidate);
        }
    }
    None
}

pub fn clean(
    dom: &mut RcDom,
    id: &Path,
    handle: &Handle,
    url: &Url,
    candidates: &BTreeMap<String, Candidate>,
) -> bool {
    let mut useless = false;

    match handle.data {
        Document => (),
        Doctype { .. } => (),
        Text { ref contents } => {
            let s = contents.borrow();
            if s.trim().is_empty() {
                useless = true
            }
        }
        Comment { .. } => useless = true,
        Element {
            ref name,
            ref attrs,
            ..
        } => {
            let tag_name = name.local.as_ref();

            match tag_name.to_lowercase().as_ref() {
                "script" | "link" | "style" | "noscript" | "meta" | "object" | "svg" | "footer"
                | "aside" => useless = true,
                "form" | "table" | "ul" | "div" => useless = is_useless(id, &handle, candidates),
                "img" | "picture" => useless = !fix_img_path(&handle, url),
                "a" => useless = !fix_anchor_path(&handle, url),
                _ => (),
            }
            dom::clean_attr("id", &mut attrs.borrow_mut());
            dom::clean_attr("class", &mut attrs.borrow_mut());
            dom::clean_attr("style", &mut attrs.borrow_mut());
        }
        ProcessingInstruction { .. } => useless = true,
    }
    let mut useless_nodes = vec![];
    for (i, child) in handle.children.borrow().iter().enumerate() {
        let pid = id.join(i.to_string());
        if clean(dom, pid.as_path(), &child, url, candidates) {
            useless_nodes.push(child.clone());
        }
    }
    for node in useless_nodes.iter() {
        dom.remove_from_parent(node);
    }
    if dom::is_empty(handle) {
        useless = true
    }
    useless
}

pub fn is_useless(id: &Path, handle: &Handle, candidates: &BTreeMap<String, Candidate>) -> bool {
    let tag_name = &dom::get_tag_name(handle).unwrap_or_default();
    let weight = get_class_weight(handle);
    let score = id
        .to_str()
        .and_then(|id| candidates.get(id))
        .map(|c| c.score.get())
        .unwrap_or(0.0);
    if weight + score < 0.0 {
        return true;
    }
    let text_nodes_len = dom::text_children_count(handle);

    // Count all tags in a single traversal instead of 5 separate traversals
    let mut counts = dom::TagCounts::default();
    dom::count_tags(handle, &mut counts);

    let p_count = counts.p;
    let img_count = counts.img;
    let li_count = counts.li as i32 - 100;
    let input_count = counts.input;
    let embed_count = counts.embed;
    let link_density = get_link_density(handle);
    let content_length = dom::text_len(handle);
    let para_count = text_nodes_len + p_count;

    if img_count > para_count + text_nodes_len {
        return true;
    }
    if li_count > para_count as i32 && tag_name != "ul" && tag_name != "ol" {
        return true;
    }
    if input_count as f32 > f32::floor(para_count as f32 / 3.0) {
        return true;
    }
    if content_length < 25 && (img_count == 0 || img_count > 2) {
        return true;
    }
    if weight < 25.0 && link_density > 0.2 {
        return true;
    }
    if (embed_count == 1 && content_length < 35) || embed_count > 1 {
        return true;
    }
    false
}

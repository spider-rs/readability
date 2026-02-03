use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use llm_readability::extractor;
use url::Url;

/// Simple article HTML for baseline benchmarking
fn simple_article() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Simple Article</title>
</head>
<body>
    <header>
        <nav><a href="/">Home</a> | <a href="/about">About</a></nav>
    </header>
    <article>
        <h1>The Main Article Title</h1>
        <p>This is the first paragraph of the article. It contains meaningful content that should be extracted by the readability algorithm.</p>
        <p>Here is another paragraph with more text. The algorithm should identify this as the main content area based on text density and structure.</p>
        <p>A third paragraph adds more weight to this content block, making it more likely to be selected as the top candidate.</p>
    </article>
    <footer>
        <p>Copyright 2024</p>
    </footer>
</body>
</html>"#
        .to_string()
}

/// Complex article with sidebars, ads, and nested content
fn complex_article() -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Complex Article with Sidebars</title>
    <style>.ad { display: block; }</style>
    <script>console.log("analytics");</script>
</head>
<body>
    <header class="site-header">
        <nav class="main-nav">
            <ul>
                <li><a href="/">Home</a></li>
                <li><a href="/news">News</a></li>
                <li><a href="/sports">Sports</a></li>
            </ul>
        </nav>
    </header>
    <div class="container">
        <aside class="sidebar left-sidebar">
            <div class="widget">
                <h3>Popular Posts</h3>
                <ul>
                    <li><a href="/post1">Post 1</a></li>
                    <li><a href="/post2">Post 2</a></li>
                </ul>
            </div>
            <div class="ad-break">Advertisement</div>
        </aside>
        <main class="content">
            <article class="post">
                <h1>Understanding Machine Learning: A Comprehensive Guide</h1>
                <div class="meta">Published on January 1, 2024 by Author Name</div>
"#,
    );

    // Add substantial article content
    for i in 1..=10 {
        html.push_str(&format!(
            r#"<p>This is paragraph {} of the article. Machine learning is a subset of artificial intelligence that enables systems to learn and improve from experience without being explicitly programmed. This paragraph contains technical content that demonstrates the algorithm's ability to identify substantive text.</p>"#,
            i
        ));
    }

    html.push_str(
        r#"
                <blockquote>
                    <p>"Machine learning is the field of study that gives computers the ability to learn without being explicitly programmed." - Arthur Samuel</p>
                </blockquote>
                <h2>Key Concepts</h2>
                <ul>
                    <li>Supervised Learning</li>
                    <li>Unsupervised Learning</li>
                    <li>Reinforcement Learning</li>
                </ul>
            </article>
        </main>
        <aside class="sidebar right-sidebar">
            <div class="ad sponsor">Sponsored Content</div>
            <div class="related-posts">
                <h4>Related Articles</h4>
                <a href="/related1">Related 1</a>
                <a href="/related2">Related 2</a>
            </div>
        </aside>
    </div>
    <footer class="site-footer">
        <div class="footer-links">
            <a href="/privacy">Privacy</a>
            <a href="/terms">Terms</a>
        </div>
        <p class="copyright">Copyright 2024 Example Site</p>
    </footer>
    <script>trackPageView();</script>
</body>
</html>"#,
    );

    html
}

/// Deeply nested HTML structure
fn nested_html(depth: usize) -> String {
    let mut html = String::from("<!DOCTYPE html><html><head><title>Nested</title></head><body>");

    for _ in 0..depth {
        html.push_str("<div>");
    }

    html.push_str("<article><h1>Deep Content</h1>");
    html.push_str("<p>This content is deeply nested within multiple div elements. The readability algorithm should still be able to find and extract it correctly.</p>");
    html.push_str("<p>Additional paragraph to add weight to this content block and ensure it's selected as the main content.</p>");
    html.push_str("</article>");

    for _ in 0..depth {
        html.push_str("</div>");
    }

    html.push_str("</body></html>");
    html
}

/// HTML with many links (high link density)
fn link_heavy_html() -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head><title>Link Heavy Page</title></head>
<body>
<nav>"#,
    );

    for i in 0..50 {
        html.push_str(&format!(r#"<a href="/link{}">Link {}</a> "#, i, i));
    }

    html.push_str(
        r#"</nav>
<main>
<article>
<h1>Article with Good Content</h1>
<p>Despite the heavy navigation above, this article contains the main content that should be extracted. The readability algorithm uses link density as one metric to filter out navigation-heavy sections.</p>
<p>This second paragraph reinforces the content area. Good readability extraction should identify this article section as the primary content despite the high link count in the navigation.</p>
<p>A third paragraph with substantial text helps the scoring algorithm identify this as the main content block.</p>
</article>
</main>
</body>
</html>"#,
    );

    html
}

/// Generate large HTML document
fn large_document(paragraph_count: usize) -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Large Document</title>
</head>
<body>
<article>
<h1>A Very Long Article</h1>
"#,
    );

    for i in 0..paragraph_count {
        html.push_str(&format!(
            "<p>Paragraph {} of the long document. This contains enough text to be meaningful for the content scoring algorithm. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.</p>\n",
            i + 1
        ));
    }

    html.push_str("</article></body></html>");
    html
}

/// Multi-language content
fn multilang_html() -> String {
    r#"<!DOCTYPE html>
<html lang="ja">
<head>
    <meta charset="utf-8">
    <title>多言語コンテンツ</title>
</head>
<body>
<article>
    <h1>日本語の記事</h1>
    <p>これは日本語で書かれた記事です。読みやすさアルゴリズムは、日本語の句読点も認識する必要があります。</p>
    <p>二番目の段落には、より多くのコンテンツが含まれています。アルゴリズムは、テキストの密度と構造に基づいてメインコンテンツエリアを識別する必要があります。</p>
    <p>三番目の段落は、このコンテンツブロックにさらに重みを加えます。</p>
</article>
</body>
</html>"#
        .to_string()
}

fn bench_simple_extraction(c: &mut Criterion) {
    let html = simple_article();
    let url = Url::parse("https://example.com/article").unwrap();

    c.bench_function("extract_simple_article", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_complex_extraction(c: &mut Criterion) {
    let html = complex_article();
    let url = Url::parse("https://example.com/article").unwrap();

    c.bench_function("extract_complex_article", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_nested_depth(c: &mut Criterion) {
    let url = Url::parse("https://example.com/nested").unwrap();

    let mut group = c.benchmark_group("nested_depth");
    for depth in [5, 10, 20, 50].iter() {
        let html = nested_html(*depth);
        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(depth), &html, |b, html| {
            b.iter(|| {
                let mut input = black_box(html.as_bytes());
                extractor::extract(&mut input, &url).unwrap()
            })
        });
    }
    group.finish();
}

fn bench_link_density(c: &mut Criterion) {
    let html = link_heavy_html();
    let url = Url::parse("https://example.com/links").unwrap();

    c.bench_function("extract_link_heavy", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_document_size(c: &mut Criterion) {
    let url = Url::parse("https://example.com/large").unwrap();

    let mut group = c.benchmark_group("document_size");
    for para_count in [10, 50, 100, 500].iter() {
        let html = large_document(*para_count);
        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(para_count),
            &html,
            |b, html| {
                b.iter(|| {
                    let mut input = black_box(html.as_bytes());
                    extractor::extract(&mut input, &url).unwrap()
                })
            },
        );
    }
    group.finish();
}

fn bench_multilang(c: &mut Criterion) {
    let html = multilang_html();
    let url = Url::parse("https://example.com/ja/article").unwrap();

    c.bench_function("extract_japanese", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

/// HTML with tables
fn table_heavy_html() -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head><title>Data Table Article</title></head>
<body>
<article>
<h1>Quarterly Sales Report</h1>
<p>Below is the comprehensive sales data for the current quarter broken down by region.</p>
<table>
<thead>
<tr><th>Region</th><th>Q1</th><th>Q2</th><th>Q3</th><th>Q4</th></tr>
</thead>
<tbody>
"#,
    );

    for i in 0..20 {
        html.push_str(&format!(
            "<tr><td>Region {}</td><td>${}</td><td>${}</td><td>${}</td><td>${}</td></tr>\n",
            i,
            i * 1000 + 500,
            i * 1200 + 600,
            i * 1100 + 550,
            i * 1300 + 700
        ));
    }

    html.push_str(
        r#"</tbody>
</table>
<p>This data represents our year-over-year growth in each market segment.</p>
<p>Additional analysis shows strong performance across all metrics.</p>
</article>
</body>
</html>"#,
    );

    html
}

/// HTML with code blocks
fn code_heavy_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head><title>Technical Tutorial</title></head>
<body>
<article>
<h1>Getting Started with Rust</h1>
<p>This tutorial covers the basics of Rust programming language.</p>
<h2>Hello World</h2>
<p>Let's start with a simple example:</p>
<pre><code>fn main() {
    println!("Hello, world!");
}
</code></pre>
<p>The code above demonstrates the entry point of a Rust program.</p>
<h2>Variables and Types</h2>
<p>Rust has a strong type system:</p>
<pre><code>let x: i32 = 42;
let name: &amp;str = "Rust";
let is_awesome: bool = true;

fn add(a: i32, b: i32) -> i32 {
    a + b
}
</code></pre>
<p>Variables are immutable by default in Rust.</p>
<h2>Error Handling</h2>
<p>Rust uses Result and Option types for error handling:</p>
<pre><code>fn divide(a: f64, b: f64) -> Result&lt;f64, String&gt; {
    if b == 0.0 {
        Err("Cannot divide by zero".to_string())
    } else {
        Ok(a / b)
    }
}
</code></pre>
<p>This pattern eliminates null pointer exceptions.</p>
</article>
</body>
</html>"#
        .to_string()
}

/// HTML with many images
fn image_heavy_html() -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head><title>Photo Gallery</title></head>
<body>
<article>
<h1>Nature Photography Collection</h1>
<p>A curated collection of stunning nature photographs from around the world.</p>
"#,
    );

    for i in 0..30 {
        html.push_str(&format!(
            r#"<figure>
<img src="/images/photo{}.jpg" alt="Nature photo {}">
<figcaption>Beautiful landscape scene number {}</figcaption>
</figure>
"#,
            i, i, i
        ));
    }

    html.push_str(
        r#"<p>All photographs were taken during expeditions across five continents.</p>
<p>Each image captures the raw beauty of our natural world.</p>
</article>
</body>
</html>"#,
    );

    html
}

/// Malformed HTML
fn malformed_html() -> String {
    r#"<!DOCTYPE html>
<html>
<head><title>Malformed Page
<body>
<article>
<h1>Unclosed Tags Everywhere
<p>This paragraph has no closing tag
<p>Neither does this one
<div>
<span>Nested unclosed span
<p>Another paragraph inside div
</article>
<p>Paragraph outside article but not closed
<div class="footer>Missing quote in attribute
<p>Content continues despite errors
</body>
</html>"#
        .to_string()
}

/// HTML with special characters and entities
fn special_chars_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head><title>Special Characters</title></head>
<body>
<article>
<h1>Character Encoding &amp; Entities</h1>
<p>HTML entities: &lt;tag&gt; &amp; &quot;quotes&quot; &apos;apostrophe&apos;</p>
<p>Unicode: café, naïve, résumé, über, 日本語, 中文, العربية, עברית</p>
<p>Math symbols: α β γ δ ∑ ∏ √ ∫ ≈ ≠ ≤ ≥ ∞</p>
<p>Currency: $ € £ ¥ ₹ ₽ ₿</p>
<p>Arrows: ← → ↑ ↓ ↔ ⇒ ⇐</p>
<p>This tests the parser's ability to handle diverse character sets correctly.</p>
</article>
</body>
</html>"#
        .to_string()
}

/// Very wide HTML (many siblings)
fn wide_html(sibling_count: usize) -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html>
<head><title>Wide Document</title></head>
<body>
<article>
<h1>Article with Many Sections</h1>
"#,
    );

    for i in 0..sibling_count {
        html.push_str(&format!(
            "<section><h2>Section {}</h2><p>Content for section {} with enough text to be meaningful.</p></section>\n",
            i, i
        ));
    }

    html.push_str("</article></body></html>");
    html
}

fn bench_table_extraction(c: &mut Criterion) {
    let html = table_heavy_html();
    let url = Url::parse("https://example.com/report").unwrap();

    c.bench_function("extract_table_heavy", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_code_extraction(c: &mut Criterion) {
    let html = code_heavy_html();
    let url = Url::parse("https://example.com/tutorial").unwrap();

    c.bench_function("extract_code_heavy", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_image_extraction(c: &mut Criterion) {
    let html = image_heavy_html();
    let url = Url::parse("https://example.com/gallery").unwrap();

    c.bench_function("extract_image_heavy", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_malformed(c: &mut Criterion) {
    let html = malformed_html();
    let url = Url::parse("https://example.com/malformed").unwrap();

    c.bench_function("extract_malformed", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_special_chars(c: &mut Criterion) {
    let html = special_chars_html();
    let url = Url::parse("https://example.com/unicode").unwrap();

    c.bench_function("extract_special_chars", |b| {
        b.iter(|| {
            let mut input = black_box(html.as_bytes());
            extractor::extract(&mut input, &url).unwrap()
        })
    });
}

fn bench_wide_document(c: &mut Criterion) {
    let url = Url::parse("https://example.com/wide").unwrap();

    let mut group = c.benchmark_group("wide_document");
    for sibling_count in [10, 50, 100, 200].iter() {
        let html = wide_html(*sibling_count);
        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(sibling_count),
            &html,
            |b, html| {
                b.iter(|| {
                    let mut input = black_box(html.as_bytes());
                    extractor::extract(&mut input, &url).unwrap()
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_simple_extraction,
    bench_complex_extraction,
    bench_nested_depth,
    bench_link_density,
    bench_document_size,
    bench_multilang,
    bench_table_extraction,
    bench_code_extraction,
    bench_image_extraction,
    bench_malformed,
    bench_special_chars,
    bench_wide_document,
);

criterion_main!(benches);

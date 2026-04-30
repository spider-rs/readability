pub mod dom;
pub mod error;
pub mod extractor;
pub mod rcdom;
pub mod scorer;

#[cfg(all(test, feature = "tokio"))]
mod async_tests {
    use super::error::Error;
    use super::extractor;
    use std::io;
    use tokio::io::{AsyncRead, ReadBuf};

    /// Compile-time assertion that the async futures are `Send`. This is the
    /// whole point of depending on `spider-html5ever` / `spider-tendril`: the
    /// returned futures hold the parser across `.await` points, and the
    /// parser stack must be `Send` for `tokio::spawn` on a multi-threaded
    /// runtime to compile.
    #[test]
    fn async_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}

        let url = url::Url::parse("https://example.com/").unwrap();
        let bytes = b"<!doctype html><html><body><p>x</p></body></html>".to_vec();
        let fut = extractor::extract_async(bytes, url.clone());
        assert_send(&fut);

        // The reader future is `Send` whenever the reader itself is `Send`.
        // tokio::io::Empty is Send, so this composes.
        let reader = tokio::io::empty();
        let fut = extractor::extract_async_reader(reader, url);
        assert_send(&fut);
    }

    /// AsyncRead that yields a fixed payload one tiny chunk at a time.
    /// Exercises the streaming sink path under fragmented reads.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            let remaining = self.data.len() - self.pos;
            if remaining == 0 {
                return std::task::Poll::Ready(Ok(()));
            }
            let n = remaining.min(self.chunk).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.data[start..start + n]);
            self.pos += n;
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// AsyncRead that errors on the second poll. Exercises the IO-error
    /// branch of the streaming loop.
    struct FlakyReader {
        data: Vec<u8>,
        pos: usize,
        first_poll: bool,
    }

    impl AsyncRead for FlakyReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if !self.first_poll {
                return std::task::Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, "flaky")));
            }
            self.first_poll = false;
            let remaining = self.data.len() - self.pos;
            let n = remaining.min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.data[start..start + n]);
            self.pos += n;
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn small_html() -> String {
        r#"<!DOCTYPE html><html lang="en"><head><title>Tiny</title></head>
<body><article><h1>Tiny Heading</h1>
<p>First paragraph with sufficient prose for the readability scorer to consider it.</p>
<p>Second paragraph adds weight so this article wins as the top candidate.</p>
<p>Third paragraph for additional content scoring.</p>
</article></body></html>"#
            .to_string()
    }

    fn large_html() -> String {
        // Build a payload >> ASYNC_BYTE_THRESHOLD (128 KiB) to force the
        // streaming/spawn_blocking path.
        let mut out = String::from(
            r#"<!DOCTYPE html><html lang="en"><head><title>Big Article</title></head><body><article><h1>Big Heading</h1>"#,
        );
        for i in 0..2000 {
            out.push_str(&format!(
                "<p>Paragraph number {} with enough text to score, lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.</p>",
                i
            ));
        }
        out.push_str("</article></body></html>");
        assert!(
            out.len() > extractor::ASYNC_BYTE_THRESHOLD,
            "fixture must exceed threshold"
        );
        out
    }

    #[tokio::test]
    async fn extract_async_small_inline() {
        let html = small_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        let product = extractor::extract_async(html.into_bytes(), url)
            .await
            .unwrap();
        assert!(product.content.contains("Tiny Heading"));
        assert!(product.text.contains("First paragraph"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn extract_async_large_offloaded() {
        let html = large_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        let product = extractor::extract_async(html.into_bytes(), url)
            .await
            .unwrap();
        assert!(product.content.contains("Big Heading"));
        assert!(product.text.contains("Paragraph number 0"));
        assert!(product.text.contains("Paragraph number 1999"));
    }

    #[tokio::test]
    async fn extract_async_reader_small_inline() {
        let html = small_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        let reader = ChunkedReader {
            data: html.into_bytes(),
            pos: 0,
            chunk: 1024,
        };
        let product = extractor::extract_async_reader(reader, url).await.unwrap();
        assert!(product.content.contains("Tiny Heading"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn extract_async_reader_large_streaming() {
        let html = large_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        // Tiny chunk size to stress the streaming sink and channel backpressure.
        let reader = ChunkedReader {
            data: html.into_bytes(),
            pos: 0,
            chunk: 4096,
        };
        let product = extractor::extract_async_reader(reader, url).await.unwrap();
        assert!(product.content.contains("Big Heading"));
        assert!(product.text.contains("Paragraph number 0"));
        assert!(product.text.contains("Paragraph number 1999"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn extract_async_reader_io_error_after_threshold() {
        // Payload larger than threshold so we cross into the streaming path,
        // then the reader errors on the next poll.
        let html = large_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        let reader = FlakyReader {
            data: html.into_bytes(),
            pos: 0,
            first_poll: true,
        };
        let result = extractor::extract_async_reader(reader, url).await;
        // Either we read everything before the error (if the first poll
        // covered the whole body) or we surface the IO error. Both are
        // acceptable; the critical guarantee is that the future completes
        // — no hang, no panic.
        match result {
            Ok(_) => {}
            Err(Error::IOError(_)) => {}
            Err(Error::Unexpected) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn extract_async_matches_sync_output() {
        let html = small_html();
        let url = url::Url::parse("https://example.com/").unwrap();
        let sync_product = extractor::extract(&mut html.as_bytes(), &url).unwrap();
        let async_product = extractor::extract_async(html.into_bytes(), url)
            .await
            .unwrap();
        assert_eq!(sync_product.content, async_product.content);
        assert_eq!(sync_product.text, async_product.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_readability() {
        use maud::{html, DOCTYPE};

        let page_title = "Readability Test";
        let page_h1 = "Reading is fun";

        let markup = html! {
            (DOCTYPE)
            html lang="fr" {
                meta charset="utf-8";
                title { (page_title) }
                h1 { (page_h1) }
                a href="spider.cloud";
                pre {
                    r#"The content is ready for reading"#
                }
            }
        }
        .into_string();

        match extractor::extract(
            &mut markup.as_bytes(),
            &url::Url::parse("https://spider.cloud").unwrap(),
        ) {
            Ok(product) => {
                assert!(
                    product
                        .content
                        .contains(&format!("<title>{}</title>", page_title)),
                    "Title is missing or incorrect"
                );
                assert!(
                    product.content.contains(&format!("<h1>{page_h1}</h1>")),
                    "H1 tag is missing or incorrect"
                );
                assert!(
                    product.content.contains("The content is ready for reading"),
                    "Expected phrase is missing"
                );
                assert!(
                    product
                        .content
                        .contains(&r###"<html class="paper" lang="fr">"###),
                    "Html lang is missing or incorrect"
                );
            }
            Err(_) => println!("error occured"),
        }
    }

    #[test]
    fn test_extract_article_content() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Test Article</title></head>
<body>
    <header><nav>Navigation links here</nav></header>
    <article>
        <h1>Main Article Heading</h1>
        <p>This is the first paragraph of the main article content. It should be extracted.</p>
        <p>This is the second paragraph with more substantive content for the reader.</p>
        <p>A third paragraph adds weight to ensure this is identified as main content.</p>
    </article>
    <footer>Footer content</footer>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com/article").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("Main Article Heading"));
        assert!(result.content.contains("first paragraph"));
        assert!(result.content.contains("second paragraph"));
        assert!(result.text.contains("Main Article Heading"));
    }

    #[test]
    fn test_extract_preserves_title() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>My Page Title</title></head>
<body>
    <article>
        <h1>Article Heading</h1>
        <p>Content paragraph one with enough text to be meaningful.</p>
        <p>Content paragraph two with additional text for scoring.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("<title>My Page Title</title>"));
    }

    #[test]
    fn test_extract_removes_scripts() {
        let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Page with Scripts</title>
    <script>alert('malicious');</script>
</head>
<body>
    <article>
        <h1>Clean Article</h1>
        <p>This content should be clean without any script tags.</p>
        <p>Another paragraph to add weight to the content block.</p>
        <script>console.log('inline script');</script>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        // Note: The library adds its own <script>window.isReaderPage = true;</script>
        // so we check for the malicious content being removed, not the absence of all scripts
        assert!(!result.content.contains("malicious"));
        assert!(!result.content.contains("inline script"));
        assert!(result.content.contains("Clean Article"));
    }

    #[test]
    fn test_extract_removes_styles() {
        let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Page with Styles</title>
    <style>.hidden { display: none; }</style>
</head>
<body>
    <article>
        <h1>Styled Article</h1>
        <p>Content without inline styles in the output.</p>
        <p>More content to ensure proper extraction.</p>
    </article>
    <style>body { color: red; }</style>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(!result.content.contains("<style>"));
        assert!(!result.content.contains("display: none"));
        assert!(result.content.contains("Styled Article"));
    }

    #[test]
    fn test_extract_handles_nested_divs() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Nested Divs</title></head>
<body>
    <div class="wrapper">
        <div class="container">
            <div class="content">
                <article>
                    <h1>Deeply Nested Content</h1>
                    <p>This paragraph is nested several levels deep in div elements.</p>
                    <p>Another paragraph to add content weight for scoring.</p>
                </article>
            </div>
        </div>
    </div>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("Deeply Nested Content"));
        assert!(result.text.contains("nested several levels deep"));
    }

    #[test]
    fn test_extract_language_attribute() {
        let html = r#"<!DOCTYPE html>
<html lang="de">
<head><title>German Article</title></head>
<body>
    <article>
        <h1>Deutscher Artikel</h1>
        <p>Dies ist ein deutscher Artikel mit genug Text für die Extraktion.</p>
        <p>Ein weiterer Absatz um das Gewicht zu erhöhen.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.de").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains(r#"lang="de""#));
    }

    #[test]
    fn test_extract_fixes_relative_image_urls() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Images</title></head>
<body>
    <article>
        <h1>Article with Images</h1>
        <p>Here is an image: <img src="/images/photo.jpg" alt="Photo"></p>
        <p>More content to ensure this is identified as the main article.</p>
        <p>Additional text paragraph for scoring weight.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com/articles/test").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result
            .content
            .contains("https://example.com/images/photo.jpg"));
    }

    #[test]
    fn test_extract_fixes_relative_anchor_urls() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Links</title></head>
<body>
    <article>
        <h1>Article with Links</h1>
        <p>Check out <a href="/other-article">this other article</a> for more info.</p>
        <p>More content to give this section enough weight to be extracted.</p>
        <p>Third paragraph for additional scoring weight in the algorithm.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com/articles/test").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("https://example.com/other-article"));
    }

    #[test]
    fn test_extract_handles_empty_content() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Empty Page</title></head>
<body></body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url);

        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_removes_sidebar_content() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Page with Sidebar</title></head>
<body>
    <div class="sidebar">
        <p>Sidebar content that should be removed or deprioritized.</p>
    </div>
    <article class="main-content">
        <h1>Main Article</h1>
        <p>This is the main content that should be extracted and prioritized.</p>
        <p>Another paragraph to add weight to this content section.</p>
        <p>Third paragraph ensures this block scores higher than the sidebar.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("Main Article"));
        assert!(result.text.contains("main content"));
    }

    #[test]
    fn test_extract_removes_ad_content() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Page with Ads</title></head>
<body>
    <div class="ad-break">
        <p>Advertisement content</p>
    </div>
    <article>
        <h1>Article Without Ads</h1>
        <p>The main article content should be free of advertisements.</p>
        <p>More substantive content for the readability algorithm.</p>
        <p>Additional paragraph to ensure proper content scoring.</p>
    </article>
    <div class="sponsor">Sponsored content</div>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("Article Without Ads"));
    }

    #[test]
    fn test_extract_handles_blockquotes() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Article with Quote</title></head>
<body>
    <article>
        <h1>Article Title</h1>
        <p>Introduction paragraph with some context.</p>
        <blockquote>
            <p>This is an important quote that should be preserved in the output.</p>
        </blockquote>
        <p>Conclusion paragraph wrapping up the article.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("blockquote"));
        assert!(result.text.contains("important quote"));
    }

    #[test]
    fn test_extract_handles_lists() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Article with Lists</title></head>
<body>
    <article>
        <h1>Article with Lists</h1>
        <p>Here are some key points about the topic:</p>
        <ul>
            <li>First important point</li>
            <li>Second important point</li>
            <li>Third important point</li>
        </ul>
        <p>And here is a numbered list of steps:</p>
        <ol>
            <li>Step one</li>
            <li>Step two</li>
            <li>Step three</li>
        </ol>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.text.contains("First important point"));
        assert!(result.text.contains("Step one"));
    }

    #[test]
    fn test_extract_preserves_headings_hierarchy() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Article with Headings</title></head>
<body>
    <article>
        <h1>Main Title</h1>
        <p>Introduction to the article with substantial content.</p>
        <h2>Section One</h2>
        <p>Content for section one with meaningful text.</p>
        <h2>Section Two</h2>
        <p>Content for section two with more information.</p>
        <h3>Subsection</h3>
        <p>Detailed content in the subsection area.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("<h1>Main Title</h1>"));
        assert!(result.content.contains("<h2>Section One</h2>"));
        assert!(result.content.contains("<h2>Section Two</h2>"));
    }

    #[test]
    fn test_extract_handles_preformatted_text() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Code Article</title></head>
<body>
    <article>
        <h1>Code Example</h1>
        <p>Here is a code example that demonstrates the concept:</p>
        <pre>
fn main() {
    println!("Hello, world!");
}
        </pre>
        <p>The code above shows a simple Rust program.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("<pre>"));
        assert!(result.text.contains("println!"));
    }

    #[test]
    fn test_extract_japanese_content() {
        let html = r#"<!DOCTYPE html>
<html lang="ja">
<head><title>日本語記事</title></head>
<body>
    <article>
        <h1>日本語の見出し</h1>
        <p>これは日本語で書かれた記事です。日本語の句読点（。、！？）を含みます。</p>
        <p>二番目の段落には、さらに多くの内容があります。</p>
        <p>三番目の段落は、コンテンツブロックに重みを加えます。</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.jp").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("日本語の見出し"));
        assert!(result.content.contains(r#"lang="ja""#));
    }

    #[test]
    fn test_extract_chinese_content() {
        let html = r#"<!DOCTYPE html>
<html lang="zh">
<head><title>中文文章</title></head>
<body>
    <article>
        <h1>中文标题</h1>
        <p>这是一篇中文文章。它包含中文标点符号，如句号。和逗号，</p>
        <p>第二段包含更多的内容和信息。</p>
        <p>第三段增加了文章的权重。</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.cn").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("中文标题"));
        assert!(result.content.contains(r#"lang="zh""#));
    }

    #[test]
    fn test_extract_removes_comments() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Page with Comments</title></head>
<body>
    <!-- This is an HTML comment that should be removed -->
    <article>
        <h1>Article Title</h1>
        <!-- Another comment inside the article -->
        <p>Main content paragraph that should be preserved.</p>
        <p>Second paragraph with more content for scoring.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(!result.content.contains("<!--"));
        assert!(!result.content.contains("HTML comment"));
        assert!(result.content.contains("Article Title"));
    }

    #[test]
    fn test_extract_removes_noscript() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Page with Noscript</title></head>
<body>
    <noscript>
        <p>JavaScript is required for this page.</p>
    </noscript>
    <article>
        <h1>Main Article</h1>
        <p>Content that should be extracted normally.</p>
        <p>More content for the readability algorithm to process.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(!result.content.contains("<noscript>"));
        assert!(!result.content.contains("JavaScript is required"));
        assert!(result.content.contains("Main Article"));
    }

    #[test]
    fn test_extract_base_url_in_output() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Test</title></head>
<body>
    <article>
        <h1>Test Article</h1>
        <p>Content for the article with enough text to be extracted.</p>
        <p>Additional content paragraph for scoring purposes.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com/path/to/article").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result
            .content
            .contains(r#"<base href="https://example.com/path/to/article">"#));
    }

    #[test]
    fn test_extract_text_output() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Text Test</title></head>
<body>
    <article>
        <h1>Plain Text Test</h1>
        <p>This text should appear in the plain text output.</p>
        <p>So should this second paragraph of content.</p>
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.text.contains("Plain Text Test"));
        assert!(result.text.contains("should appear in the plain text"));
        assert!(!result.text.contains("<p>"));
        assert!(!result.text.contains("<h1>"));
    }

    #[test]
    fn test_extract_handles_malformed_html() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Malformed</title>
<body>
    <article>
        <h1>Unclosed Tags
        <p>Missing closing tags but still valid enough to parse.
        <p>Another paragraph without closing tag.
    </article>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url);

        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_removes_footer() {
        let html = r#"<!DOCTYPE html>
<html>
<head><title>Page with Footer</title></head>
<body>
    <article>
        <h1>Main Article Content</h1>
        <p>This is the main article that should be extracted.</p>
        <p>More content to ensure proper extraction and scoring.</p>
        <p>Third paragraph for additional weight in the algorithm.</p>
    </article>
    <footer>
        <p>Copyright 2024 Example Inc. All rights reserved.</p>
        <nav>
            <a href="/privacy">Privacy</a>
            <a href="/terms">Terms</a>
        </nav>
    </footer>
</body>
</html>"#;

        let url = url::Url::parse("https://example.com").unwrap();
        let result = extractor::extract(&mut html.as_bytes(), &url).unwrap();

        assert!(result.content.contains("Main Article Content"));
        assert!(!result.content.contains("Copyright 2024"));
    }

    #[test]
    fn test_product_debug() {
        let product = extractor::Product {
            content: String::from("<html>test</html>"),
            text: String::from("test"),
        };

        let debug_str = format!("{:?}", product);
        assert!(debug_str.contains("Product"));
        assert!(debug_str.contains("content"));
        assert!(debug_str.contains("text"));
    }
}

#[cfg(test)]
mod dom_tests {
    use super::*;
    use crate::rcdom::RcDom;
    use html5ever::parse_document;
    use html5ever::tendril::TendrilSink;

    fn parse_html(html: &str) -> RcDom {
        parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .unwrap()
    }

    fn find_element<'a>(
        handle: &'a crate::rcdom::Handle,
        tag: &str,
    ) -> Option<crate::rcdom::Handle> {
        if let Some(name) = dom::get_tag_name(handle) {
            if name == tag {
                return Some(handle.clone());
            }
        }
        for child in handle.children.borrow().iter() {
            if let Some(found) = find_element(child, tag) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_get_tag_name() {
        let dom = parse_html("<html><body><div>test</div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert_eq!(dom::get_tag_name(&div), Some("div".to_string()));
    }

    #[test]
    fn test_get_attr() {
        let dom =
            parse_html(r#"<html><body><div id="main" class="container">test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        assert_eq!(dom::get_attr("id", &div), Some("main".to_string()));
        assert_eq!(dom::get_attr("class", &div), Some("container".to_string()));
        assert_eq!(dom::get_attr("nonexistent", &div), None);
    }

    #[test]
    fn test_extract_text() {
        let dom = parse_html("<html><body><p>Hello <strong>World</strong>!</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let mut text = String::new();
        dom::extract_text(&p, &mut text, true);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn test_extract_text_shallow() {
        let dom = parse_html("<html><body><p>Hello <strong>World</strong>!</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let mut text = String::new();
        dom::extract_text(&p, &mut text, false);
        assert!(text.contains("Hello"));
        assert!(!text.contains("World"));
    }

    #[test]
    fn test_text_len() {
        let dom = parse_html("<html><body><p>Hello World</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let len = dom::text_len(&p);
        assert_eq!(len, 11);
    }

    #[test]
    fn test_find_node() {
        let dom = parse_html(
            "<html><body><div><a href='#'>Link 1</a><a href='#'>Link 2</a></div></body></html>",
        );
        let div = find_element(&dom.document, "div").unwrap();
        let mut links = vec![];
        dom::find_node(&div, "a", &mut links);
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn test_has_link() {
        let dom = parse_html("<html><body><p>No link here</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        assert!(!dom::has_link(&p));

        let dom2 = parse_html("<html><body><p><a href='#'>Link</a></p></body></html>");
        let p2 = find_element(&dom2.document, "p").unwrap();
        assert!(dom::has_link(&p2));
    }

    #[test]
    fn test_is_empty() {
        let dom = parse_html("<html><body><div></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(dom::is_empty(&div));

        let dom2 = parse_html("<html><body><div><p>Content</p></div></body></html>");
        let div2 = find_element(&dom2.document, "div").unwrap();
        assert!(!dom::is_empty(&div2));
    }

    #[test]
    fn test_has_nodes() {
        let dom = parse_html("<html><body><div><p>Para</p></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(dom::has_nodes(&div, &vec!["p"]));
        assert!(!dom::has_nodes(&div, &vec!["span"]));
    }

    #[test]
    fn test_text_children_count() {
        let dom = parse_html("<html><body><div>Short<p></p>This is a longer text node that exceeds twenty characters</div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        let count = dom::text_children_count(&div);
        assert_eq!(count, 1);
    }
}

#[cfg(test)]
mod scorer_tests {
    use super::*;
    use crate::rcdom::RcDom;
    use html5ever::parse_document;
    use html5ever::tendril::TendrilSink;

    fn parse_html(html: &str) -> RcDom {
        parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .unwrap()
    }

    fn find_element(handle: &crate::rcdom::Handle, tag: &str) -> Option<crate::rcdom::Handle> {
        if let Some(name) = dom::get_tag_name(handle) {
            if name == tag {
                return Some(handle.clone());
            }
        }
        for child in handle.children.borrow().iter() {
            if let Some(found) = find_element(child, tag) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_is_candidate_paragraph() {
        let dom =
            parse_html("<html><body><p>This is enough text to be considered.</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        assert!(scorer::is_candidate(&p));
    }

    #[test]
    fn test_is_candidate_short_text() {
        let dom = parse_html("<html><body><p>Hi</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        assert!(!scorer::is_candidate(&p));
    }

    #[test]
    fn test_get_class_weight_positive() {
        let dom =
            parse_html(r#"<html><body><div class="article content">Test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        let weight = scorer::get_class_weight(&div);
        assert!(weight > 0.0);
    }

    #[test]
    fn test_get_class_weight_negative() {
        let dom =
            parse_html(r#"<html><body><div class="sidebar comment">Test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        let weight = scorer::get_class_weight(&div);
        assert!(weight < 0.0);
    }

    #[test]
    fn test_get_class_weight_neutral() {
        let dom = parse_html(r#"<html><body><div class="wrapper">Test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        let weight = scorer::get_class_weight(&div);
        assert_eq!(weight, 0.0);
    }

    #[test]
    fn test_init_content_score_article() {
        let dom = parse_html("<html><body><article>Test content</article></body></html>");
        let article = find_element(&dom.document, "article").unwrap();
        let score = scorer::init_content_score(&article);
        assert!(score >= 10.0);
    }

    #[test]
    fn test_init_content_score_form() {
        let dom = parse_html("<html><body><form>Test content</form></body></html>");
        let form = find_element(&dom.document, "form").unwrap();
        let score = scorer::init_content_score(&form);
        assert!(score <= -3.0);
    }

    #[test]
    fn test_calc_content_score() {
        let dom = parse_html("<html><body><p>This is a sentence. Here is another one! And a question?</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let score = scorer::calc_content_score(&p);
        assert!(score > 1.0);
    }

    #[test]
    fn test_get_link_density() {
        let dom = parse_html(
            "<html><body><div>Regular text <a href='#'>link</a> more text</div></body></html>",
        );
        let div = find_element(&dom.document, "div").unwrap();
        let density = scorer::get_link_density(&div);
        assert!(density > 0.0);
        assert!(density < 1.0);
    }

    #[test]
    fn test_get_link_density_no_links() {
        let dom = parse_html("<html><body><div>Regular text without any links</div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        let density = scorer::get_link_density(&div);
        assert_eq!(density, 0.0);
    }

    #[test]
    fn test_get_link_density_all_links() {
        let dom = parse_html("<html><body><div><a href='#'>All link text</a></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        let density = scorer::get_link_density(&div);
        assert!((density - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_local_url() {
        assert!(scorer::local_url("/path/to/resource"));
        assert!(scorer::local_url("relative/path"));
        assert!(!scorer::local_url("http://example.com"));
        assert!(!scorer::local_url("https://example.com"));
        assert!(!scorer::local_url("//example.com/path"));
    }

    #[test]
    fn test_fix_img_path() {
        let dom = parse_html(r#"<html><body><img src="/images/test.jpg"></body></html>"#);
        let img = find_element(&dom.document, "img").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_img_path(&img, &url);
        assert!(result);

        let src = dom::get_attr("src", &img).unwrap();
        assert_eq!(src, "https://example.com/images/test.jpg");
    }

    #[test]
    fn test_fix_anchor_path() {
        let dom = parse_html(r#"<html><body><a href="/other-page">Link</a></body></html>"#);
        let a = find_element(&dom.document, "a").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_anchor_path(&a, &url);
        assert!(result);

        let href = dom::get_attr("href", &a).unwrap();
        assert_eq!(href, "https://example.com/other-page");
    }

    #[test]
    fn test_preprocess_extracts_title() {
        let html = "<html><head><title>Page Title</title></head><body><p>Content</p></body></html>";
        let mut dom = parse_html(html);
        let mut title = String::new();
        let mut lang = String::new();
        let handle = dom.document.clone();

        scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);

        assert_eq!(title, "Page Title");
    }

    #[test]
    fn test_preprocess_extracts_lang() {
        let html = r#"<html lang="es"><head><title>Title</title></head><body><p>Content</p></body></html>"#;
        let mut dom = parse_html(html);
        let mut title = String::new();
        let mut lang = String::new();
        let handle = dom.document.clone();

        scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);

        assert_eq!(lang, "es");
    }

    #[test]
    fn test_is_candidate_div_with_block_children() {
        let dom =
            parse_html("<html><body><div><p>Some paragraph content here</p></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(scorer::is_candidate(&div));
    }

    #[test]
    fn test_is_candidate_div_without_block_children() {
        let dom = parse_html("<html><body><div>Just some text</div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(!scorer::is_candidate(&div));
    }

    #[test]
    fn test_is_candidate_h1() {
        let dom = parse_html("<html><body><h1>Main Heading Title</h1></body></html>");
        let h1 = find_element(&dom.document, "h1").unwrap();
        assert!(scorer::is_candidate(&h1));
    }

    #[test]
    fn test_is_candidate_h2() {
        let dom = parse_html("<html><body><h2>Section Heading</h2></body></html>");
        let h2 = find_element(&dom.document, "h2").unwrap();
        assert!(scorer::is_candidate(&h2));
    }

    #[test]
    fn test_init_content_score_div() {
        let dom = parse_html("<html><body><div>Test</div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        let score = scorer::init_content_score(&div);
        assert_eq!(score, 5.0);
    }

    #[test]
    fn test_init_content_score_blockquote() {
        let dom = parse_html("<html><body><blockquote>Quote</blockquote></body></html>");
        let bq = find_element(&dom.document, "blockquote").unwrap();
        let score = scorer::init_content_score(&bq);
        assert_eq!(score, 3.0);
    }

    #[test]
    fn test_init_content_score_th() {
        let dom = parse_html("<html><body><table><tr><th>Header</th></tr></table></body></html>");
        let th = find_element(&dom.document, "th").unwrap();
        let score = scorer::init_content_score(&th);
        assert_eq!(score, 5.0);
    }

    #[test]
    fn test_init_content_score_h1() {
        let dom = parse_html("<html><body><h1>Title</h1></body></html>");
        let h1 = find_element(&dom.document, "h1").unwrap();
        let score = scorer::init_content_score(&h1);
        assert_eq!(score, 10.0);
    }

    #[test]
    fn test_get_class_weight_with_id() {
        let dom = parse_html(r#"<html><body><div id="article">Test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        let weight = scorer::get_class_weight(&div);
        assert!(weight > 0.0);
    }

    #[test]
    fn test_get_class_weight_negative_id() {
        let dom = parse_html(r#"<html><body><div id="sidebar">Test</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();
        let weight = scorer::get_class_weight(&div);
        assert!(weight < 0.0);
    }

    #[test]
    fn test_calc_content_score_japanese_punctuation() {
        let dom = parse_html(
            "<html><body><p>これは日本語です。テストです！質問ですか？</p></body></html>",
        );
        let p = find_element(&dom.document, "p").unwrap();
        let score = scorer::calc_content_score(&p);
        assert!(score > 1.0);
    }

    #[test]
    fn test_calc_content_score_chinese_punctuation() {
        let dom = parse_html("<html><body><p>这是中文。测试句子，有逗号！</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let score = scorer::calc_content_score(&p);
        assert!(score > 1.0);
    }

    #[test]
    fn test_fix_img_path_absolute_url() {
        let dom =
            parse_html(r#"<html><body><img src="https://other.com/image.jpg"></body></html>"#);
        let img = find_element(&dom.document, "img").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_img_path(&img, &url);
        assert!(result);

        let src = dom::get_attr("src", &img).unwrap();
        assert_eq!(src, "https://other.com/image.jpg");
    }

    #[test]
    fn test_fix_img_path_no_src() {
        let dom = parse_html(r#"<html><body><img alt="no src"></body></html>"#);
        let img = find_element(&dom.document, "img").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_img_path(&img, &url);
        assert!(!result);
    }

    #[test]
    fn test_fix_anchor_path_absolute_url() {
        let dom =
            parse_html(r#"<html><body><a href="https://other.com/page">Link</a></body></html>"#);
        let a = find_element(&dom.document, "a").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_anchor_path(&a, &url);
        assert!(result);

        let href = dom::get_attr("href", &a).unwrap();
        assert_eq!(href, "https://other.com/page");
    }

    #[test]
    fn test_fix_anchor_path_no_href() {
        let dom = parse_html(r#"<html><body><a name="anchor">Anchor</a></body></html>"#);
        let a = find_element(&dom.document, "a").unwrap();
        let url = url::Url::parse("https://example.com/article").unwrap();

        let result = scorer::fix_anchor_path(&a, &url);
        assert!(!result);
    }

    #[test]
    fn test_preprocess_removes_scripts() {
        let html =
            "<html><head><script>alert('test');</script></head><body><p>Content</p></body></html>";
        let mut dom = parse_html(html);
        let mut title = String::new();
        let mut lang = String::new();
        let handle = dom.document.clone();

        scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);

        assert!(find_element(&dom.document, "script").is_none());
    }

    #[test]
    fn test_preprocess_removes_styles() {
        let html = "<html><head><style>.foo { color: red; }</style></head><body><p>Content</p></body></html>";
        let mut dom = parse_html(html);
        let mut title = String::new();
        let mut lang = String::new();
        let handle = dom.document.clone();

        scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);

        assert!(find_element(&dom.document, "style").is_none());
    }

    #[test]
    fn test_preprocess_removes_links() {
        let html = r#"<html><head><link rel="stylesheet" href="style.css"></head><body><p>Content</p></body></html>"#;
        let mut dom = parse_html(html);
        let mut title = String::new();
        let mut lang = String::new();
        let handle = dom.document.clone();

        scorer::preprocess(&mut dom, &handle, &mut title, &mut lang);

        assert!(find_element(&dom.document, "link").is_none());
    }

    #[test]
    fn test_local_url_protocol_relative() {
        assert!(!scorer::local_url("//cdn.example.com/image.png"));
    }

    #[test]
    fn test_local_url_fragment() {
        assert!(scorer::local_url("#section"));
    }

    #[test]
    fn test_local_url_query_string() {
        assert!(scorer::local_url("page?query=value"));
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use std::io;

    #[test]
    fn test_error_display_url_parse() {
        let parse_err = url::Url::parse("not a url").unwrap_err();
        let err = error::Error::UrlParseError(parse_err);
        let display = format!("{}", err);
        assert!(display.contains("UrlParseError"));
    }

    #[test]
    fn test_error_display_unexpected() {
        let err = error::Error::Unexpected;
        let display = format!("{}", err);
        assert_eq!(display, "UnexpectedError");
    }

    #[test]
    fn test_error_display_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err = error::Error::IOError(io_err);
        let display = format!("{}", err);
        assert!(display.contains("InputOutputError"));
    }

    #[test]
    fn test_error_from_url_parse() {
        let parse_err = url::Url::parse(":::invalid").unwrap_err();
        let err: error::Error = parse_err.into();
        assert!(matches!(err, error::Error::UrlParseError(_)));
    }

    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let err: error::Error = io_err.into();
        assert!(matches!(err, error::Error::IOError(_)));
    }

    #[test]
    fn test_error_debug() {
        let err = error::Error::Unexpected;
        let debug = format!("{:?}", err);
        assert!(debug.contains("Unexpected"));
    }

    #[test]
    fn test_error_is_std_error() {
        use std::error::Error;
        let err = error::Error::Unexpected;
        let _: &dyn Error = &err;
    }
}

#[cfg(test)]
mod dom_attr_tests {
    use super::*;
    use crate::rcdom::RcDom;
    use html5ever::parse_document;
    use html5ever::tendril::TendrilSink;

    fn parse_html(html: &str) -> RcDom {
        parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .unwrap()
    }

    fn find_element(handle: &crate::rcdom::Handle, tag: &str) -> Option<crate::rcdom::Handle> {
        if let Some(name) = dom::get_tag_name(handle) {
            if name == tag {
                return Some(handle.clone());
            }
        }
        for child in handle.children.borrow().iter() {
            if let Some(found) = find_element(child, tag) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_set_attr() {
        let dom = parse_html(r#"<html><body><div id="test">Content</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();

        dom::set_attr("id", "new-id", &div);
        let id = dom::get_attr("id", &div).unwrap();
        assert_eq!(id, "new-id");
    }

    #[test]
    fn test_set_attr_nonexistent() {
        let dom = parse_html(r#"<html><body><div>Content</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();

        dom::set_attr("id", "new-id", &div);
        let id = dom::get_attr("id", &div);
        assert!(id.is_none());
    }

    #[test]
    fn test_clean_attr() {
        let dom = parse_html(r#"<html><body><div class="test-class">Content</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();

        if let crate::rcdom::NodeData::Element { ref attrs, .. } = div.data {
            let mut attrs_mut = attrs.borrow_mut();
            assert!(dom::attr("class", &attrs_mut).is_some());
            dom::clean_attr("class", &mut attrs_mut);
            assert!(dom::attr("class", &attrs_mut).is_none());
        }
    }

    #[test]
    fn test_clean_attr_nonexistent() {
        let dom = parse_html(r#"<html><body><div id="test-id">Content</div></body></html>"#);
        let div = find_element(&dom.document, "div").unwrap();

        if let crate::rcdom::NodeData::Element { ref attrs, .. } = div.data {
            let mut attrs_mut = attrs.borrow_mut();
            let initial_len = attrs_mut.len();
            dom::clean_attr("class", &mut attrs_mut);
            assert_eq!(attrs_mut.len(), initial_len);
        }
    }

    #[test]
    fn test_attr_function() {
        let dom = parse_html(
            r#"<html><body><div id="my-id" class="my-class">Content</div></body></html>"#,
        );
        let div = find_element(&dom.document, "div").unwrap();

        if let crate::rcdom::NodeData::Element { ref attrs, .. } = div.data {
            let attrs_ref = attrs.borrow();
            assert_eq!(dom::attr("id", &attrs_ref), Some("my-id".to_string()));
            assert_eq!(dom::attr("class", &attrs_ref), Some("my-class".to_string()));
            assert_eq!(dom::attr("style", &attrs_ref), None);
        }
    }

    #[test]
    fn test_get_tag_name_text_node() {
        let dom = parse_html("<html><body>Text content</body></html>");
        let body = find_element(&dom.document, "body").unwrap();
        for child in body.children.borrow().iter() {
            let tag_name = dom::get_tag_name(child);
            assert!(tag_name.is_none());
        }
    }

    #[test]
    fn test_is_empty_with_whitespace() {
        let dom = parse_html("<html><body><p>   </p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        assert!(dom::is_empty(&p));
    }

    #[test]
    fn test_is_empty_with_nested_empty() {
        let dom = parse_html("<html><body><div><p></p></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(dom::is_empty(&div));
    }

    #[test]
    fn test_text_len_unicode() {
        let dom = parse_html("<html><body><p>日本語テスト</p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let len = dom::text_len(&p);
        assert_eq!(len, 6);
    }

    #[test]
    fn test_text_len_with_whitespace() {
        let dom = parse_html("<html><body><p>  hello world  </p></body></html>");
        let p = find_element(&dom.document, "p").unwrap();
        let len = dom::text_len(&p);
        assert_eq!(len, 11);
    }

    #[test]
    fn test_find_node_nested() {
        let dom =
            parse_html("<html><body><div><div><a href='#'>Link</a></div></div></body></html>");
        let body = find_element(&dom.document, "body").unwrap();
        let mut links = vec![];
        dom::find_node(&body, "a", &mut links);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn test_has_nodes_multiple_tags() {
        let dom = parse_html("<html><body><div><span>Text</span></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        assert!(dom::has_nodes(&div, &vec!["span", "p"]));
        assert!(!dom::has_nodes(&div, &vec!["p", "a"]));
    }

    #[test]
    fn test_extract_text_deep_nesting() {
        let dom =
            parse_html("<html><body><div><span><em>Deep</em> text</span></div></body></html>");
        let div = find_element(&dom.document, "div").unwrap();
        let mut text = String::new();
        dom::extract_text(&div, &mut text, true);
        assert!(text.contains("Deep"));
        assert!(text.contains("text"));
    }
}

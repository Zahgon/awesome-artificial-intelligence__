//! Rust port of `tests/test_validate_readme.py`.
//!
//! Each test mirrors the original 1:1 — same inputs, same assertions.

use validate_readme::{
    classify_exception, classify_status, normalize_url, validate_churn, validate_text, LinkError,
    Severity,
};

const VALID: &str = "# List\n\n### Books\n\n- [A Book](https://example.com/book): A useful book.\n";

/// `any('needle' in e for e in errors)`
fn any_contains(errors: &[String], needle: &str) -> bool {
    errors.iter().any(|e| e.contains(needle))
}

#[test]
fn test_valid_resource() {
    let result = validate_text(VALID);
    assert_eq!(result.resources.len(), 1);
    assert_eq!(result.errors, Vec::<String>::new());
    assert_eq!(result.warnings, Vec::<String>::new());
}

#[test]
fn test_malformed_resource() {
    // http:// fails RESOURCE_RE which requires https://
    let result = validate_text("### Books\n\n- [A Book](http://example.com): No TLS.\n");
    assert!(result.errors[0].contains("malformed resource entry"));
}

#[test]
fn test_duplicate_title_and_normalized_url() {
    let text = "### Books\n\n\
- [A Book](https://EXAMPLE.com/book/): First entry.\n\
- [a book](https://example.com/book#section): Second entry.\n";
    let result = validate_text(text);
    assert!(any_contains(&result.errors, "duplicate title"));
    assert!(any_contains(&result.errors, "duplicate URL"));
}

#[test]
fn test_empty_category() {
    let result = validate_text("### Books\n\nSome prose.\n");
    assert!(result.errors[0].contains("category 'Books' has no resources"));
}

#[test]
fn test_level_two_heading_resets_category() {
    let text = format!(
        "{VALID}\n## Contributing\n\n- [A Tool](https://example.com/tool): A tool.\n"
    );
    let result = validate_text(&text);
    assert!(any_contains(&result.errors, "outside a level-three category"));
}

#[test]
fn test_same_category_name_in_different_sections() {
    let text = "## First\n\n### Tools\n\n## Second\n\n### Tools\n\n\
- [A Tool](https://example.com/tool): A tool.\n";
    let result = validate_text(text);
    assert!(any_contains(&result.errors, "section 'First'"));
}

#[test]
fn test_description_needs_period() {
    let result = validate_text(
        "### Books\n\n- [A Book](https://example.com/book): Missing punctuation\n",
    );
    assert!(result.errors[0].contains("description must end with a period"));
}

#[test]
fn test_normalize_url() {
    assert_eq!(
        normalize_url("HTTPS://EXAMPLE.COM:443/path/?q=1#fragment").unwrap(),
        "https://example.com/path?q=1"
    );
}

#[test]
fn test_invalid_url_is_an_error() {
    let result =
        validate_text("### Books\n\n- [A Book](https://example.com:bad/book): Invalid port.\n");
    assert!(result.errors[0].contains("invalid URL"));
}

#[test]
fn test_link_status_classification() {
    let url = "https://example.com";
    assert_eq!(classify_status(404, url).unwrap().0, Severity::Error);
    assert_eq!(classify_status(400, url).unwrap().0, Severity::Error);
    assert_eq!(classify_status(451, url).unwrap().0, Severity::Error);
    assert_eq!(classify_status(403, url).unwrap().0, Severity::Warning);
    assert_eq!(classify_status(408, url).unwrap().0, Severity::Warning);
    assert_eq!(classify_status(503, url).unwrap().0, Severity::Warning);
    assert!(classify_status(200, url).is_none());
}

#[test]
fn test_link_exception_classification() {
    let url = "https://example.invalid";
    // dns = URLError(socket.gaierror("not found"))
    assert_eq!(
        classify_exception(LinkError::Dns, url, "not found").0,
        Severity::Error
    );
    // tls = URLError(ssl.SSLError("bad certificate"))
    assert_eq!(
        classify_exception(LinkError::Tls, url, "bad certificate").0,
        Severity::Error
    );
    // timeout = URLError(TimeoutError("timed out"))
    assert_eq!(
        classify_exception(LinkError::Timeout, url, "timed out").0,
        Severity::Warning
    );
}

#[test]
fn test_churn_limits() {
    let tools: Vec<String> = (0..6)
        .map(|i| format!("- [Tool {i}](https://example.com/{i}): A tool."))
        .collect();
    let base = format!(
        "## Learn\n\n### Books\n\n- [Book](https://example.com/book): A book.\n\n## Build\n\n### Tools\n\n{}",
        tools.join("\n")
    );

    // acceptable = base.replace("A tool.", "A better tool.", 6)
    let acceptable = replace_n(&base, "A tool.", "A better tool.", 6);
    assert_eq!(validate_churn(&base, &acceptable), Vec::<String>::new());

    // too_many = acceptable + "\n- [Tool 7](https://example.com/7): A tool.\n"
    let too_many = format!("{acceptable}\n- [Tool 7](https://example.com/7): A tool.\n");
    assert!(any_contains(
        &validate_churn(&base, &too_many),
        "resource entries"
    ));

    // four_additions = base + "\n" + "\n".join(f"- [New {i}]..." for i in range(4))
    let additions: Vec<String> = (0..4)
        .map(|i| format!("- [New {i}](https://example.com/new-{i}): A tool."))
        .collect();
    let four_additions = format!("{base}\n{}", additions.join("\n"));
    assert!(any_contains(
        &validate_churn(&base, &four_additions),
        "net entries"
    ));

    // two_foundations: revise the Learn book AND add a second Learn book.
    let two_foundations = base
        .replace("A book.", "A revised book.")
        .replace(
            "\n## Build",
            "\n- [Second Book](https://example.com/book-2): A book.\n\n## Build",
        );
    assert!(any_contains(
        &validate_churn(&base, &two_foundations),
        "foundational entries"
    ));

    // moved_to_foundations: revise the Learn book AND move Tool 0 into Learn.
    let moved_to_foundations = base.replace("A book.", "A revised book.").replace(
        "\n## Build\n\n### Tools\n\n- [Tool 0](https://example.com/0): A tool.",
        "\n- [Tool 0](https://example.com/0): A tool.\n\n## Build\n\n### Tools",
    );
    assert!(any_contains(
        &validate_churn(&base, &moved_to_foundations),
        "foundational entries"
    ));
}

/// Equivalent of Python's `str.replace(old, new, count)` (replace first `count`
/// occurrences only).
fn replace_n(haystack: &str, old: &str, new: &str, count: usize) -> String {
    let mut result = String::new();
    let mut remaining = haystack;
    let mut done = 0;
    while done < count {
        match remaining.find(old) {
            Some(idx) => {
                result.push_str(&remaining[..idx]);
                result.push_str(new);
                remaining = &remaining[idx + old.len()..];
                done += 1;
            }
            None => break,
        }
    }
    result.push_str(remaining);
    result
}

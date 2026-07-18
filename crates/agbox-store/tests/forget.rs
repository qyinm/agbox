#![allow(clippy::unwrap_used)]

use agbox_store::fts_literal_query;

#[test]
fn hostile_fts_syntax_becomes_bounded_literal_terms() {
    let expression = fts_literal_query("alpha OR beta* \"quoted\"").unwrap();
    assert_eq!(
        expression,
        "\"alpha\" AND \"OR\" AND \"beta*\" AND \"\"\"quoted\"\"\""
    );
    assert!(fts_literal_query("   ").is_err());
    assert!(fts_literal_query(&"a ".repeat(600)).is_err());
}

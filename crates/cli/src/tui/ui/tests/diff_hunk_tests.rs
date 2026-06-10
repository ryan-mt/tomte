use super::super::{diff_hunk, DiffRow};

fn tag(r: &DiffRow<'_>) -> (char, String) {
    match *r {
        DiffRow::Context(l) => (' ', l.to_string()),
        DiffRow::Del(l) => ('-', l.to_string()),
        DiffRow::Add(l) => ('+', l.to_string()),
    }
}

#[test]
fn shared_anchor_lines_collapse_to_context() {
    // One changed line inside a 3-line block: the unchanged first and last
    // lines become context, not a removed+added echo of the whole block.
    let old = "fn f() {\n    let x = 1;\n}";
    let new = "fn f() {\n    let x = 2;\n}";
    let rows: Vec<_> = diff_hunk(old, new).iter().map(tag).collect();
    assert_eq!(
        rows,
        vec![
            (' ', "fn f() {".to_string()),
            ('-', "    let x = 1;".to_string()),
            ('+', "    let x = 2;".to_string()),
            (' ', "}".to_string()),
        ]
    );
}

#[test]
fn pure_insertion_and_deletion_have_no_phantom_context() {
    let add: Vec<_> = diff_hunk("", "new line").iter().map(tag).collect();
    assert_eq!(add, vec![('+', "new line".to_string())]);
    let del: Vec<_> = diff_hunk("gone", "").iter().map(tag).collect();
    assert_eq!(del, vec![('-', "gone".to_string())]);
}

#[test]
fn fully_distinct_blocks_keep_every_line() {
    // No shared anchors: all old lines removed, all new lines added, in order.
    let rows: Vec<_> = diff_hunk("alpha\nbeta", "gamma\ndelta")
        .iter()
        .map(tag)
        .collect();
    assert_eq!(
        rows,
        vec![
            ('-', "alpha".to_string()),
            ('-', "beta".to_string()),
            ('+', "gamma".to_string()),
            ('+', "delta".to_string()),
        ]
    );
}

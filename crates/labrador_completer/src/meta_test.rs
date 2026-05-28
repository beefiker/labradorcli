use super::*;

/*
0 1 2 3 4 5 6 7
l a b r a d o r
----------------
0             8  << the span for the string "labrador" is (0, 8)

Spanned {
    item: String::new("labrador"),  << labrador string
    span: Span::new(0, 8)           << span
}

or >> String::new("labrador").spanned(Span::new(0, 8))        */
fn labrador() -> Spanned<String> {
    String::from("labrador").spanned(Span::new(0, 8))
}

fn empty() -> Spanned<String> {
    String::new().spanned_unknown()
}

#[test]
fn knows_distances() {
    assert!(labrador().span.distance() == 8);
    assert!(empty().span.distance() == 0);
}

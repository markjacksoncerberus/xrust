// Regression: XPath positional predicates over the smite tree.
// Guards the per-context-node predicate evaluation fix (numeric predicates,
// last(), per-parent grouping, and positional predicates on all axes).
use chadpath::item::{Item, Node, Sequence};
use chadpath::parser::xml::parse as xmlparse;
use chadpath::parser::xpath::parse;
use chadpath::parser::ParseError;
use chadpath::transform::context::{ContextBuilder, StaticContextBuilder};
use chadpath::trees::smite::RNode;
use chadpath::xdmerror::{Error, ErrorKind};

fn run(xml: &str, xp: &str) -> String {
    let doc = RNode::new_document();
    xmlparse(doc.clone(), xml, Some(|_: &_| Err(ParseError::MissingNameSpace))).expect("xml");
    let mut st = StaticContextBuilder::new()
        .message(|_| Ok(()))
        .fetcher(|_| Err(Error::new(ErrorKind::NotImplemented, "")))
        .parser(|_| Err(Error::new(ErrorKind::NotImplemented, "")))
        .build();
    let t = parse::<RNode>(xp, None, None).expect("xpath parse");
    let seq: Sequence<RNode> = ContextBuilder::new()
        .context(vec![Item::Node(doc.clone())])
        .result_document(RNode::new_document())
        .build()
        .dispatch(&mut st, &t)
        .unwrap_or_default();
    seq.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

#[test]
fn positional_predicates() {
    let one = "<a><x>1</x><x>2</x><x>3</x></a>";
    let two = "<r><a><x>1</x><x>2</x></a><a><x>3</x><x>4</x></a></r>";
    let cases = [
        (one, "/a/x[1]", "1"),
        (one, "/a/x[2]", "2"),
        (one, "/a/x[position()=2]", "2"),
        (one, "/a/x[last()]", "3"),
        (one, "/a/x[last()-1]", "2"),
        (two, "//a/x[1]", "1,3"),                                  // per-parent grouping
        (two, "//a/x[position()=1]", "1,3"),
        (one, "/a/x[1]/following-sibling::x[1]", "2"),             // positional on fwd axis
        (one, "/a/x[3]/preceding-sibling::x[1]", "2"),            // positional on rev axis
        (one, "(/a/x[1]/following-sibling::x)[1]", "2"),          // FilterExpr index
    ];
    for (xml, xp, exp) in cases {
        assert_eq!(run(xml, xp), exp, "xpath: {xp}");
    }
}

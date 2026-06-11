//! Navigation routines

use crate::Item;
use crate::item::{Node, NodeType, Sequence, SequenceTrait};
use crate::transform::context::{Context, StaticContext};
use crate::transform::{Axis, NodeMatch, Transform};
use crate::value::ValueData;
use crate::xdmerror::{Error, ErrorKind};
use crate::names::{NcName, QName};
use std::cmp::Ordering;
use url::Url;

/// The root node of the context item.
pub(crate) fn root<N: Node>(ctxt: &Context<N>) -> Result<Sequence<N>, Error> {
    ctxt.context_item.as_ref().map_or(
        Err(Error::new(
            ErrorKind::ContextNotNode,
            String::from("no context"),
        )),
        |i| match &i {
            Item::Node(n) => match n.node_type() {
                NodeType::Document => Ok(vec![Item::Node(n.clone())]),
                _ => n
                    .ancestor_iter()
                    .last()
                    .map_or(Ok(vec![]), |m| Ok(vec![Item::Node(m)])),
            },
            _ => Err(Error::new(
                ErrorKind::ContextNotNode,
                String::from("context item is not a node"),
            )),
        },
    )
}

/// The context item.
pub(crate) fn context<N: Node>(ctxt: &Context<N>) -> Result<Sequence<N>, Error> {
    ctxt.context_item.as_ref().map_or(
        Err(Error::new(
            ErrorKind::DynamicAbsent,
            String::from("no context"),
        )),
        |i| Ok(vec![i.clone()]),
    )
}

/// Each transform in the supplied vector is evaluated.
/// The sequence returned by a transform is used as the context for the next transform.
/// See also XSLT 20.4.1 for how the current item is set.
pub(crate) fn compose<
    N: Node,
    F: FnMut(&str) -> Result<(), Error>,
    G: FnMut(&str) -> Result<N, Error>,
    H: FnMut(&Url) -> Result<String, Error>,
>(
    ctxt: &Context<N>,
    stctxt: &mut StaticContext<N, F, G, H>,
    steps: &Vec<Transform<N>>,
) -> Result<Sequence<N>, Error> {
    // PERF: evolve a single `Context` in place across steps instead of rebuilding
    // it with `ContextBuilder::from(&context)` every iteration. That `From` deep-
    // clones the whole `Context` — *and* clones the context sequence a second time
    // for `current` — only for the caller to immediately overwrite both with
    // `.context(new)` / `.current(previous)`. Because predicates are themselves
    // `Compose`s evaluated once per context node, those wasted full-context clones
    // multiplied into the dominant per-query allocation churn (tens of thousands
    // of clones for a single `//a[…]//b[…]/text()` over a few-hundred-node page).
    //
    // This reproduces the exact field outcome of the old
    // `from(&context).context(new).current(previous).build()`:
    //   * context      ← new          (and context_item ← new[0] iff new non-empty)
    //   * current      ← previous
    //   * current_item ← the *pre-step* context_item (None if it was None)
    //   * i ← 0, last ← None; all other fields (templates/vars/namespaces/…) carry
    // — but without cloning anything beyond the `previous` sequence the semantics
    // actually require.
    let mut context = ctxt.clone();
    for t in steps.iter() {
        // `current` for this step is always the *original* context sequence
        // (XSLT 20.4.1), not the evolving one.
        let previous = ctxt.context.clone();
        let new = context.dispatch(stctxt, t)?;
        // `from(&context)` set current_item from the context item that was in
        // force *before* `.context(new)` replaced it — capture it first.
        let new_current_item = context.context_item.clone();
        // `.context(new)`: context_item only changes when `new` is non-empty.
        if let Some(first) = new.first() {
            context.context_item = Some(first.clone());
        }
        context.context = new;
        context.i = 0;
        context.last = None;
        // `.current(previous)` and the current_item carried from `from`.
        context.current = previous;
        context.current_item = new_current_item;
    }
    Ok(context.context)
}

/// For each item in the current context, evaluate the given node matching operation.
pub(crate) fn step<N: Node>(ctxt: &Context<N>, nm: &NodeMatch) -> Result<Sequence<N>, Error> {
    match ctxt.context.iter().try_fold(vec![], |mut acc, i| {
        match i {
            Item::Node(n) => {
                match nm.axis {
                    Axis::SelfAxis => {
                        if nm.matches(n) {
                            acc.push(i.clone());
                            Ok(acc)
                        } else {
                            Ok(acc)
                        }
                    }
                    Axis::SelfDocument => {
                        if n.node_type() == NodeType::Document {
                            acc.push(i.clone());
                            Ok(acc)
                        } else {
                            Ok(acc)
                        }
                    }
                    Axis::Child => {
                        let mut s = n.child_iter().filter(|c| nm.matches(c)).fold(
                            Sequence::new(),
                            |mut c, a| {
                                c.push_node(&a);
                                c
                            },
                        );
                        acc.append(&mut s);
                        Ok(acc)
                    }
                    Axis::Parent => match n.parent() {
                        Some(p) => {
                            acc.push_node(&p);
                            Ok(acc)
                        }
                        None => Ok(acc),
                    },
                    Axis::ParentDocument => {
                        // Only matches the Document.
                        // If no parent then return the Document
                        // NB. Document is a special kind of Node
                        match n.node_type() {
                            NodeType::Document => {
                                // The context is the document
                                acc.push(i.clone());
                                Ok(acc)
                            }
                            _ => Ok(acc),
                        }
                    }
                    Axis::Descendant => {
                        n.descend_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));

                        Ok(acc)
                    }
                    Axis::DescendantOrSelf => {
                        if nm.matches(n) {
                            acc.push(i.clone())
                        }
                        n.descend_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));
                        Ok(acc)
                    }
                    Axis::DescendantOrSelfOrRoot => {
                        acc.push_node(&n.owner_document());
                        if nm.matches(n) {
                            acc.push(i.clone())
                        }
                        n.descend_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));
                        Ok(acc)
                    }
                    Axis::Ancestor => {
                        n.ancestor_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));

                        Ok(acc)
                    }
                    Axis::AncestorOrSelf => {
                        n.ancestor_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));
                        if nm.matches(n) {
                            acc.push(i.clone())
                        }
                        Ok(acc)
                    }
                    Axis::FollowingSibling => {
                        n.next_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));

                        Ok(acc)
                    }
                    Axis::PrecedingSibling => {
                        n.prev_iter()
                            .filter(|c| nm.matches(c))
                            .for_each(|c| acc.push_node(&c));

                        Ok(acc)
                    }
                    Axis::Following => {
                        // XPath 3.3.2.1: the following axis contains all nodes that are descendants of the root of the tree in which the context node is found, are not descendants of the context node, and occur after the context node in document order.
                        // iow, for each ancestor-or-self node, include every next sibling and its descendants

                        let mut bcc = vec![];

                        // Start with following siblings of self
                        n.next_iter().for_each(|a| {
                            bcc.push(a.clone());
                            a.descend_iter().for_each(|b| bcc.push(b.clone()));
                        });

                        // Now traverse ancestors
                        n.ancestor_iter().for_each(|a| {
                            a.next_iter().for_each(|b| {
                                bcc.push(b.clone());
                                b.descend_iter().for_each(|c| bcc.push(c.clone()));
                            })
                        });
                        bcc.iter().filter(|e| nm.matches(*e)).for_each(|g| {
                            acc.push_node(g);
                        });
                        Ok(acc)
                    }
                    Axis::Preceding => {
                        // XPath 3.3.2.1: the preceding axis contains all nodes that are descendants of the root of the tree in which the context node is found, are not ancestors of the context node, and occur before the context node in document order.
                        // iow, for each ancestor-or-self node, include every previous sibling and its descendants

                        let mut bcc = vec![];

                        // Start with preceding siblings of self
                        n.prev_iter().for_each(|a| {
                            bcc.push(a.clone());
                            a.descend_iter().for_each(|b| bcc.push(b.clone()));
                        });

                        // Now traverse ancestors
                        n.ancestor_iter().for_each(|a| {
                            a.prev_iter().for_each(|b| {
                                bcc.push(b.clone());
                                b.descend_iter().for_each(|c| bcc.push(c.clone()));
                            })
                        });
                        bcc.iter().filter(|e| nm.matches(*e)).for_each(|g| {
                            acc.push_node(g);
                        });
                        Ok(acc)
                    }
                    Axis::Attribute => {
                        n.attribute_iter()
                            .filter(|a| nm.matches(a))
                            .for_each(|a| acc.push_node(&a));
                        Ok(acc)
                    }
                    Axis::SelfAttribute => {
                        if n.node_type() == NodeType::Attribute {
                            acc.push_node(n)
                        }
                        Ok(acc)
                    }
                    _ => Err(Error::new(
                        ErrorKind::NotImplemented,
                        String::from("coming soon"),
                    )),
                }
            }
            _ => Err(Error::new_with_code(
                ErrorKind::DynamicAbsent,
                "context item is not a node",
                Some(QName::from_local_name(
                    NcName::try_from("XTTE0510").unwrap(),
                )),
            )),
        }
    }) {
        Ok(mut r) => {
            // Sort in document order
            r.sort_unstable_by(|a, b| {
                get_node_unchecked(a).cmp_document_order(get_node_unchecked(b))
            });
            // Eliminate duplicates
            r.dedup_by(|a, b| {
                get_node(a).map_or(false, |aa| get_node(b).is_ok_and(|bb| aa.is_same(bb)))
            });
            Ok(r)
        }
        Err(err) => Err(err),
    }
}

fn get_node_unchecked<N: Node>(i: &Item<N>) -> &N {
    match i {
        Item::Node(n) => n,
        _ => panic!("not a node"),
    }
}
fn get_node<N: Node>(i: &Item<N>) -> Result<&N, Error> {
    match i {
        Item::Node(n) => Ok(n),
        _ => Err(Error::new(ErrorKind::Unknown, String::from("not a node"))),
    }
}

/// Remove items in the context that don't match the predicate. Used for the
/// `(expr)[predicate]` (FilterExpr) form, where the predicate applies over the
/// whole context node-set.
pub(crate) fn filter<
    N: Node,
    F: FnMut(&str) -> Result<(), Error>,
    G: FnMut(&str) -> Result<N, Error>,
    H: FnMut(&Url) -> Result<String, Error>,
>(
    ctxt: &Context<N>,
    stctxt: &mut StaticContext<N, F, G, H>,
    predicate: &Transform<N>,
) -> Result<Sequence<N>, Error> {
    let seq = ctxt.context.clone();
    let current = ctxt.current.clone();
    let current_item = ctxt.current_item.clone();
    filter_seq(ctxt, stctxt, seq, predicate, current, current_item)
}

/// Filter a node-list `seq` by one predicate, with `position()`/`last()`
/// relative to `seq` and XPath 1.0 §2.4 numeric-predicate semantics: when the
/// predicate value is a number `N` it means `position()=N`; otherwise the
/// predicate's effective boolean value is used.
pub(crate) fn filter_seq<
    N: Node,
    F: FnMut(&str) -> Result<(), Error>,
    G: FnMut(&str) -> Result<N, Error>,
    H: FnMut(&Url) -> Result<String, Error>,
>(
    ctxt: &Context<N>,
    stctxt: &mut StaticContext<N, F, G, H>,
    seq: Sequence<N>,
    predicate: &Transform<N>,
    current: Sequence<N>,
    current_item: Option<Item<N>>,
) -> Result<Sequence<N>, Error> {
    // The inner focus is each item of `seq` in turn; last() reports the size of
    // `seq`. `current`/`current_item` (the XSLT current node) are supplied by
    // the caller.
    //
    // PERF: derive the predicate-evaluation context **once** and mutate only the
    // per-item focus fields between dispatches. The previous code rebuilt it with
    // `ContextBuilder::from(ctxt)` inside the loop, and that `From` deep-clones
    // the whole `Context` — including the outer context/current node sequences —
    // on every item. For a step predicate over an N-node list that is O(N²) deep
    // clones (the chadselect fleet's `//tag[@attr=…]` CPU blowup). `dispatch`
    // borrows `&Context`, so reusing one Context across items is sound.
    let len = seq.len();
    // PERF: `ctxt.clone()` rather than `ContextBuilder::from(ctxt)`. `from` clones
    // the context sequence a *second* time to seed `current`, but we overwrite
    // `current`/`current_item` immediately below, so that clone is pure waste.
    let mut inner = ctxt.clone();
    inner.current = current;
    inner.current_item = current_item;
    inner.last = Some(len);
    seq.iter().enumerate().try_fold(vec![], |mut acc, (j, i)| {
        // PERF: reuse the 1-element context Vec's capacity across items instead
        // of allocating a fresh `vec![i.clone()]` for every node being filtered.
        // `dispatch` only ever reads (or clones) the context, never retains a
        // reference to it, so clearing and re-pushing is sound. For a predicate
        // over an N-node list this turns N allocations into one.
        inner.context.clear();
        inner.context.push(i.clone());
        inner.context_item = Some(i.clone());
        inner.i = j;
        let result = inner.dispatch(stctxt, predicate)?;
        let keep = match predicate_number(&result) {
            Some(n) => n == (j as f64 + 1.0),
            None => result.to_bool(),
        };
        if keep {
            acc.push(i.clone());
        }
        Ok(acc)
    })
}

/// If a predicate evaluated to a single numeric atomic value, return it.
/// Booleans, strings, and node-sets yield `None` (they use boolean value).
fn predicate_number<N: Node>(seq: &Sequence<N>) -> Option<f64> {
    if seq.len() != 1 {
        return None;
    }
    match &seq[0] {
        Item::Value(v) => match v.value_ref() {
            ValueData::Decimal(_)
            | ValueData::Float(_)
            | ValueData::Double(_)
            | ValueData::Integer(_)
            | ValueData::NonPositiveInteger(_)
            | ValueData::NegativeInteger(_)
            | ValueData::Long(_)
            | ValueData::Int(_)
            | ValueData::Short(_)
            | ValueData::Byte(_)
            | ValueData::NonNegativeInteger(_)
            | ValueData::UnsignedLong(_)
            | ValueData::UnsignedInt(_)
            | ValueData::UnsignedShort(_)
            | ValueData::UnsignedByte(_)
            | ValueData::PositiveInteger(_)
            | ValueData::Numeric => Some(v.to_double()),
            _ => None,
        },
        _ => None,
    }
}

/// Evaluate an axis step together with its predicate list. The predicates are
/// applied **per context node** — for each node in the context, the axis result
/// is computed and the predicates filter that per-node node-list (so
/// `position()`/`last()` are relative to each parent's matches). Results are
/// merged into a single document-ordered, duplicate-free node-set.
pub(crate) fn step_predicated<
    N: Node,
    F: FnMut(&str) -> Result<(), Error>,
    G: FnMut(&str) -> Result<N, Error>,
    H: FnMut(&Url) -> Result<String, Error>,
>(
    ctxt: &Context<N>,
    stctxt: &mut StaticContext<N, F, G, H>,
    nm: &NodeMatch,
    predicates: &[Transform<N>],
) -> Result<Sequence<N>, Error> {
    // Reverse axes number position() from the context node outward, so the
    // axis result must be ordered nearest-context-first before predicates run.
    let reverse = matches!(
        nm.axis,
        Axis::Ancestor
            | Axis::AncestorOrSelf
            | Axis::AncestorOrSelfOrRoot
            | Axis::Preceding
            | Axis::PrecedingSibling
    );
    let mut acc: Sequence<N> = Vec::new();
    // PERF: derive the per-context-node evaluation context **once**, then mutate
    // only the focus fields per node. Previously `ContextBuilder::from(ctxt)` ran
    // inside the loop and deep-cloned the whole (possibly N-node) context every
    // iteration → O(N²). `single.current` is emptied because `step` never reads
    // it and the predicates get their `current` explicitly via `filter_seq`.
    //
    // `ctxt.clone()` (not `ContextBuilder::from(ctxt)`): `from` would clone the
    // context sequence a second time to seed `current`, which we immediately
    // empty — pure waste. And the per-node `single.context` Vec is reused
    // (clear + push) rather than reallocated each iteration.
    let mut single = ctxt.clone();
    single.current = Vec::new();
    single.current_item = None;
    for it in ctxt.context.iter() {
        single.context.clear();
        single.context.push(it.clone());
        single.context_item = Some(it.clone());
        single.i = 0;
        single.last = None;
        let mut nodes = step(&single, nm)?;
        // Order in axis order so position()/last() are correct regardless of
        // the node adapter's iterator order.
        nodes.sort_by(|a, b| {
            let o = match (a, b) {
                (Item::Node(x), Item::Node(y)) => x.cmp_document_order(y),
                _ => Ordering::Equal,
            };
            if reverse {
                o.reverse()
            } else {
                o
            }
        });
        // The XSLT current() node within these predicates is the node the step
        // was taken from (the per-parent context node).
        for p in predicates {
            // Pass `&single` (cheap to clone: 1-node context, empty current) as
            // the predicate's base context — it inherits the same templates/vars/
            // namespaces as `ctxt`, and `filter_seq` overrides context/current.
            nodes = filter_seq(&single, stctxt, nodes, p, vec![it.clone()], Some(it.clone()))?;
        }
        acc.extend(nodes);
    }
    acc.sort_by(|a, b| match (a, b) {
        (Item::Node(x), Item::Node(y)) => x.cmp_document_order(y),
        _ => Ordering::Equal,
    });
    acc.dedup_by(|a, b| match (a, b) {
        (Item::Node(x), Item::Node(y)) => x.is_same(y),
        _ => false,
    });
    Ok(acc)
}

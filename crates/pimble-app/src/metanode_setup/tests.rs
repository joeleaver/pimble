use super::*;
use pimble_core::metanode::MAX_LIMIT;

#[test]
fn a_setup_edits_its_source_as_a_match() {
    // A source that is not a match yet becomes one.
    let s = edited(&Source::children(), |p| p.all.push(Kind::Tag.blank()));
    assert!(s.is_match());
    let s = edited(&s, |p| p.all[0] = with_text(&p.all[0], "waiting".into()));
    assert_eq!(parts(&s).all, vec![Condition::Tag { tag: "waiting".into() }]);
    // Every kind's blank line is blank, and a typed one is not.
    for kind in Kind::ALL {
        let blank = kind.blank();
        assert_eq!(Kind::of(&blank), Some(kind));
        assert_eq!(blank.is_blank(), kind != Kind::Type, "{kind:?}");
    }
    assert!(!Condition::Field { field: "Due".into(), is: FieldOp::Set, value: None }.is_blank());
    assert!(Condition::Field { field: "Due".into(), is: FieldOp::Less, value: None }.is_blank());
    // What was written beside the query by a newer Pimble is kept.
    let mut newer = Source::matching(Vec::new());
    newer.later.insert("group".into(), serde_json::json!("x"));
    assert_eq!(edited(&newer, |p| p.limit = 5).later, newer.later);
}

#[test]
fn from_order_and_limit_read_as_chosen() {
    let own = StoreId::new();
    let other = StoreId::new();
    assert_eq!(from_key(&None, own), "store");
    assert_eq!(from_key(&Some(Within::Store(own)), own), "store");
    assert_eq!(from_key(&Some(Within::Store(other)), own), format!("store:{other}"));
    assert_eq!(within_of("store", &Some(Within::Everywhere)), None);
    assert_eq!(within_of("everywhere", &None), Some(Within::Everywhere));
    assert_eq!(within_of(&format!("store:{other}"), &None), Some(Within::Store(other)));
    // Choosing "under" again keeps the node it named.
    let under = Some(Within::Under("a/b".into()));
    assert_eq!(within_of("under", &under), under);
    assert_eq!(within_of("under", &None), Some(Within::Under(String::new())));
    assert_eq!(order_of("field", &Order::Field("Due".into())), Order::Field("Due".into()));
    assert_eq!(order_of("modified", &Order::Title), Order::Modified);
    assert_eq!(order_key(&Order::Created), "created");
    assert_eq!(limit_of("20", 50), 20);
    assert_eq!(limit_of("0", 50), 1);
    assert_eq!(limit_of("100000", 50), MAX_LIMIT);
    assert_eq!(limit_of("lots", 50), 50);
    for (key, _) in FIELD_OPS {
        assert_eq!(op_key(op_of(key)), key);
    }
}

#[test]
fn the_count_line() {
    assert_eq!(count_line(0, &[]), "It finds nothing now.");
    assert_eq!(count_line(1, &[]), "It finds 1 node now.");
    assert_eq!(count_line(7, &[Condition::Tag { tag: "x".into() }]), "It finds 7 nodes now.");
    assert_eq!(count_line(0, &[Condition::Tag { tag: String::new() }]), "Fill in each condition to find nodes.");
}

use super::common::*;
use super::super::*;

#[test]
fn list_history_orders_newest_first() {
    let (core, _dirs) = mk_core("history", 1_000_000, 1_i64 << 60);

    let ts1 = crate::util::now_ms();
    let m1 = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "older".to_string(),
            ts_ms: ts1,
        })
        .unwrap();

    let ts2 = ts1 + 10;
    let m2 = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "newer".to_string(),
            ts_ms: ts2,
        })
        .unwrap();

    let list = core.list_history(10).unwrap();
    assert!(list.len() >= 2);

    // newest first
    assert_eq!(list[0].item_id, m2.item_id);
    assert_eq!(list[1].item_id, m1.item_id);
}

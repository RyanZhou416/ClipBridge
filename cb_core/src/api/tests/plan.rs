use super::common::*;

#[test]
fn plan_requires_confirm_over_soft() {
    let (core, _dirs) = mk_core("plan", 1_000_000, 1_i64 << 60);

    // soft_text_bytes 默认 1MB：构造稍微更大的文本
    let big = "a".repeat(1 * 1024 * 1024 + 10);
    let ts = crate::util::now_ms();

    let snap = crate::clipboard::ClipboardSnapshot::Text {
        text_utf8: big,
        ts_ms: ts,
    };

    let p1 = core.plan_local_ingest(&snap, false).unwrap();
    assert_eq!(p1.needs_user_confirm, true);

    let p2 = core.plan_local_ingest(&snap, true).unwrap();
    assert_eq!(p2.needs_user_confirm, false);
}

use crate::model::ItemKind;
use super::common::*;
use super::super::*;

#[test]
fn ingest_text_smoke() {
    let (core, _dirs) = mk_core("dedup", 1_000_000, 1_i64 << 60);

    let ts = crate::util::now_ms();
    let meta = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: "hello world".to_string(),
            ts_ms: ts,
        })
        .unwrap();

    // ItemKind 没实现 Display，所以不要 meta.kind.to_string()
    // 用匹配/相等比较即可：
    assert!(matches!(meta.kind, ItemKind::Text));
}

#[test]
fn ingest_same_text_dedup_cache() {
    let (core, _dirs) = mk_core("dedup", 1_000_000, 1_i64 << 60);

    let text = "same text";
    let bytes = text.as_bytes().to_vec();
    let sha = crate::util::sha256_hex(&bytes);

    let ts1 = crate::util::now_ms();
    let _ = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: text.to_string(),
            ts_ms: ts1,
        })
        .unwrap();

    let n_cache_1 = {
        let store = core.inner.store.lock().unwrap();
        store.cache_row_count_for_sha(&sha).unwrap()
    };
    assert_eq!(n_cache_1, 1);

    let ts2 = ts1 + 1;
    let _ = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Text {
            text_utf8: text.to_string(),
            ts_ms: ts2,
        })
        .unwrap();

    let n_cache_2 = {
        let store = core.inner.store.lock().unwrap();
        store.cache_row_count_for_sha(&sha).unwrap()
    };
    assert_eq!(n_cache_2, 1);

    assert!(core.inner.cas.blob_exists(&sha));
}

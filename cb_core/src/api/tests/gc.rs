use super::common::*;

#[test]
fn gc_evicts_lru_when_over_cap() {
    // cap = 50 bytes
    let (core, _dirs) = mk_core("gc", 1_000_000, 50);

    let ts1 = crate::util::now_ms();
    let b1 = vec![1u8; 40];
    let sha1 = crate::util::sha256_hex(&b1);

    let _ = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: b1,
            mime: "image/png".to_string(),
            ts_ms: ts1,
        })
        .unwrap();

    let ts2 = ts1 + 1;
    let b2 = vec![2u8; 40];
    let sha2 = crate::util::sha256_hex(&b2);

    let _ = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: b2,
            mime: "image/png".to_string(),
            ts_ms: ts2,
        })
        .unwrap();

    // ingest 第二个后会触发 AfterIngest GC（你的 apply_ingest 里会 run_gc）:contentReference[oaicite:4]{index=4}
    // cap=50，总量=80，应该踢掉 LRU（第一个）
    assert_eq!(core.inner.cas.blob_exists(&sha1), false);
    assert_eq!(core.inner.cas.blob_exists(&sha2), true);

    let (p1, p2) = {
        let store = core.inner.store.lock().unwrap();
        (
            store.get_cache_present(&sha1).unwrap_or(false),
            store.get_cache_present(&sha2).unwrap_or(false),
        )
    };

    assert_eq!(p1, false);
    assert_eq!(p2, true);
}

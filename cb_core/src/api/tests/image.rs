use super::common::*;

#[test]
fn ingest_same_image_dedup_cache() {
    let (core, _dirs) = mk_core("image_dedup", 1_000_000, 1_i64 << 60);

    let bytes = vec![7u8; 32];
    let sha = crate::util::sha256_hex(&bytes);

    let ts1 = crate::util::now_ms();
    let _ = core
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: bytes.clone(),
            mime: "image/png".to_string(),
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
        .ingest_local_copy(crate::clipboard::ClipboardSnapshot::Image {
            bytes: bytes.clone(),
            mime: "image/png".to_string(),
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

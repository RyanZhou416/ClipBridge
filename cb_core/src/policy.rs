use crate::model::ItemKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub soft_text_bytes: i64,
    pub soft_image_bytes: i64,
    pub soft_file_total_bytes: i64,

    pub hard_text_bytes: i64,
    pub hard_image_bytes: i64,
    pub hard_file_total_bytes: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            soft_text_bytes: 1 * 1024 * 1024,              // 1MB :contentReference[oaicite:1]{index=1}
            soft_image_bytes: 30 * 1024 * 1024,            // 30MB :contentReference[oaicite:2]{index=2}
            soft_file_total_bytes: 200 * 1024 * 1024,      // 200MB :contentReference[oaicite:3]{index=3}
            hard_text_bytes: 16 * 1024 * 1024,             // 16MB :contentReference[oaicite:4]{index=4}
            hard_image_bytes: 256 * 1024 * 1024,           // 256MB :contentReference[oaicite:5]{index=5}
            hard_file_total_bytes: 2 * 1024 * 1024 * 1024, // 2GB :contentReference[oaicite:6]{index=6}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaStrategy {
    /// 仅广播 meta；正文等用户粘贴/显式拉取（Lazy Fetch）
    MetaOnlyLazy,
    /// meta 到达后自动触发一次 ensure_content_cached（仅 text 且 <= soft）
    MetaPlusAutoPrefetch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyOutcome {
    Allowed { strategy: MetaStrategy, needs_user_confirm: bool },
    RejectedHardCap { code: &'static str },
}

/// force=true 表示“用户已确认超出软限制仍要同步”
pub fn decide(kind: ItemKind, size_bytes: i64, force: bool, limits: &Limits) -> PolicyOutcome {
    // 1) hard cap：必须拒绝
    let hard = match kind {
        ItemKind::Text => limits.hard_text_bytes,
        ItemKind::Image => limits.hard_image_bytes,
        ItemKind::FileList => limits.hard_file_total_bytes,
    };
    if size_bytes > hard {
        return PolicyOutcome::RejectedHardCap { code: "ITEM_TOO_LARGE" };
    }

    // 2) soft limit：默认策略
    let soft = match kind {
        ItemKind::Text => limits.soft_text_bytes,
        ItemKind::Image => limits.soft_image_bytes,
        ItemKind::FileList => limits.soft_file_total_bytes,
    };

    if size_bytes <= soft {
        // 文档：只有 text 且 <= soft 才自动预取 :contentReference[oaicite:7]{index=7}
        let strategy = if kind == ItemKind::Text {
            MetaStrategy::MetaPlusAutoPrefetch
        } else {
            MetaStrategy::MetaOnlyLazy
        };
        return PolicyOutcome::Allowed { strategy, needs_user_confirm: false };
    }

    // 超过 soft：需要 shell 弹窗确认；若 force 则继续但不自动预取 :contentReference[oaicite:8]{index=8}
    if force {
        PolicyOutcome::Allowed { strategy: MetaStrategy::MetaOnlyLazy, needs_user_confirm: false }
    } else {
        PolicyOutcome::Allowed { strategy: MetaStrategy::MetaOnlyLazy, needs_user_confirm: true }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ItemKind;

    #[test]
    fn text_auto_prefetch_under_soft() {
        let lim = Limits::default();
        let out = decide(ItemKind::Text, lim.soft_text_bytes, false, &lim);
        assert_eq!(out, PolicyOutcome::Allowed {
            strategy: MetaStrategy::MetaPlusAutoPrefetch,
            needs_user_confirm: false
        });
    }

    #[test]
    fn reject_over_hard_cap() {
        let lim = Limits::default();
        let out = decide(ItemKind::Image, lim.hard_image_bytes + 1, true, &lim);
        assert_eq!(out, PolicyOutcome::RejectedHardCap { code: "ITEM_TOO_LARGE" });
    }
}

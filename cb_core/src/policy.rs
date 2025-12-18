use crate::model::ItemKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Limits {
    // 软限制，在尝试拉取时如果超出限制外壳会弹窗确认
    pub soft_text_bytes: i64,
    pub soft_image_bytes: i64,
    pub soft_file_total_bytes: i64,

    // 硬限制，在复制时超出了限制就不会广播元数据
    pub hard_text_bytes: i64,
    pub hard_image_bytes: i64,
    pub hard_file_total_bytes: i64,

    // 文字自动拉取限制，超出了就不会在分析元数据的同时发送正文
    pub text_auto_prefetch_bytes: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            soft_text_bytes: 1 * 1024 * 1024,              // 1MB
            soft_image_bytes: 30 * 1024 * 1024,            // 30MB
            soft_file_total_bytes: 300 * 1024 * 1024,      // 300MB
            hard_text_bytes: 16 * 1024 * 1024,             // 16MB
            hard_image_bytes: 256 * 1024 * 1024,           // 256MB
            hard_file_total_bytes: 2 * 1024 * 1024 * 1024, // 2GB
            text_auto_prefetch_bytes: 256 * 1024,          // 256KB
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaStrategy {
    /// 仅广播 meta；正文等用户粘贴/显式拉取（Lazy Fetch）
    MetaOnlyLazy,
    /// 广播 meta 并传输正文, 到达后自动触发一次 ensure_content_cached（仅 text 且 <= soft）
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
        let strategy = if kind == ItemKind::Text && size_bytes <= limits.text_auto_prefetch_bytes {
            MetaStrategy::MetaPlusAutoPrefetch
        } else {
            MetaStrategy::MetaOnlyLazy
        };
        return PolicyOutcome::Allowed { strategy, needs_user_confirm: false };
    }


    // 超过 soft：需要 shell 弹窗确认；若 force 则继续但不自动预取
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
    #[test]
    fn text_auto_prefetch_under_threshold() {
        let lim = Limits::default();
        let out = decide(ItemKind::Text, lim.text_auto_prefetch_bytes, false, &lim);
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

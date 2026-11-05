// app/src/main/aidl/android/content/IClipboard.aidl
//
// 重要（Binder 事务码契约）：
// AIDL 生成的事务码 = FIRST_CALL_TRANSACTION + 方法声明下标，所以「方法集合 + 声明顺序」
// 必须与平台侧真实的 android.content.IClipboard.aidl 完全一致。
// 永远不要删除或重排已有方法；平台 API level 提升、新增方法时只能在末尾追加。
// 否则 getPrimaryClip 会落到 clearPrimaryClip 之类的事务码上（读剪贴板变成清剪贴板）。
//
// 本文件按 compileSdk = 36 校验，与下面两个分支的 IClipboard.aidl 一致：
//   refs/tags/android-16.0.0_r1 与 refs/heads/android16-release
//   https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-16.0.0_r1/core/java/android/content/IClipboard.aidl
// 仅省略平台侧的 @EnforcePermission 注解：注解不参与事务码，且本地 AIDL 编译路径没有该注解定义。
package android.content;

import android.content.ClipData;
import android.content.ClipDescription;
import android.content.IOnPrimaryClipChangedListener;

interface IClipboard {
    // [声明下标 0 → 事务码 1]
    void setPrimaryClip(in ClipData clip, String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 1 → 事务码 2] 平台侧带 @EnforcePermission("SET_CLIP_SOURCE")，不影响事务码
    void setPrimaryClipAsPackage(in ClipData clip, String callingPackage, String attributionTag, int userId, int deviceId, String sourcePackage);

    // [声明下标 2 → 事务码 3]
    void clearPrimaryClip(String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 3 → 事务码 4] ShizukuClipboardManager.getPrimaryClip 必须落在这里
    ClipData getPrimaryClip(String pkg, String attributionTag, int userId, int deviceId);

    // [声明下标 4 → 事务码 5]
    ClipDescription getPrimaryClipDescription(String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 5 → 事务码 6]
    boolean hasPrimaryClip(String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 6 → 事务码 7] addPrimaryClipChangedListener 必须落在这里
    void addPrimaryClipChangedListener(in IOnPrimaryClipChangedListener listener, String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 7 → 事务码 8]
    void removePrimaryClipChangedListener(in IOnPrimaryClipChangedListener listener, String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 8 → 事务码 9]
    boolean hasClipboardText(String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 9 → 事务码 10] 平台侧带 @EnforcePermission("SET_CLIP_SOURCE")
    String getPrimaryClipSource(String callingPackage, String attributionTag, int userId, int deviceId);

    // [声明下标 10 → 事务码 11]
    boolean areClipboardAccessNotificationsEnabledForUser(int userId);

    // [声明下标 11 → 事务码 12]
    void setClipboardAccessNotificationsEnabledForUser(boolean enable, int userId);
}

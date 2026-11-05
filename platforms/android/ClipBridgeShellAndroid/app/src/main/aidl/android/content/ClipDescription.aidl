// app/src/main/aidl/android/content/ClipDescription.aidl
package android.content;

// IClipboard.getPrimaryClipDescription 的签名依赖它；这里只声明为 Parcelable，
// 实际类由 android.jar 提供（与 ClipData.aidl 的处理方式一致）。
parcelable ClipDescription;

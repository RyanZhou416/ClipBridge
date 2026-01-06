// app/src/main/aidl/android/content/IOnPrimaryClipChangedListener.aidl
package android.content;

// 系统回调接口
oneway interface IOnPrimaryClipChangedListener {
    void dispatchPrimaryClipChanged();
}

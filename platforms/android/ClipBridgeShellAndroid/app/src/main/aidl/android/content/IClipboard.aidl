// app/src/main/aidl/android/content/IClipboard.aidl
package android.content;

import android.content.ClipData;
import android.content.IOnPrimaryClipChangedListener;

interface IClipboard {
    // Android 14 完整签名：
    // callingPackage, attributionTag, userId, deviceId

    void setPrimaryClip(in ClipData clip, String callingPackage, String attributionTag, int userId, int deviceId);

    ClipData getPrimaryClip(String pkg, String attributionTag, int userId, int deviceId);

    boolean hasPrimaryClip(String pkg, String attributionTag, int userId, int deviceId);

    void addPrimaryClipChangedListener(in IOnPrimaryClipChangedListener listener, String callingPackage, String attributionTag, int userId, int deviceId);

    void removePrimaryClipChangedListener(in IOnPrimaryClipChangedListener listener, String callingPackage, String attributionTag, int userId, int deviceId);
}

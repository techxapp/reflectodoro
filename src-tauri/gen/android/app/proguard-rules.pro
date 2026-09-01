# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# OverlayJsBridge is registered as window.AndroidBridge and called by method
# name from native_overlay.html's JS (see addJavascriptInterface in
# NativeOverlayManager.kt and the class's own doc comment there) --
# minification only started actually running in a shipped build once release
# CI stopped using --debug, so this keep rule is what stops R8 from
# renaming/stripping those methods.
-keepclassmembers class com.reflectodoro.app.OverlayJsBridge {
    @android.webkit.JavascriptInterface public *;
}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile
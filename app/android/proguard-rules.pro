# Gratia ProGuard rules

# Keep JNA classes used by UniFFI-generated bindings
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-dontwarn com.sun.jna.**

# Keep UniFFI-generated bindings
-keep class uniffi.gratia_ffi.** { *; }

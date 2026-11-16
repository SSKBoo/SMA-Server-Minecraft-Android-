# Protección obligatoria de símbolos JNI y puente nativo Rust
-keep class com.bedrockdroid.rs.nativebridge.** { *; }
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}
-keepclassmembers class com.bedrockdroid.rs.service.ServerForegroundService { *; }

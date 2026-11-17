# Protección obligatoria de símbolos JNI y puente nativo Rust
-keep class com.bedrockdroid.rs.nativebridge.** { *; }
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}
-keepclassmembers class com.bedrockdroid.rs.service.ServerForegroundService { *; }
# Reglas R8 Compose: evita lock verification warnings en SnapshotStateList/Map
-keepclassmembers class androidx.compose.runtime.snapshots.SnapshotStateList { *; }
-keepclassmembers class androidx.compose.runtime.snapshots.SnapshotStateMap { *; }
-dontwarn androidx.compose.runtime.snapshots.**

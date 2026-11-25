plugins {
    id("com.android.application") version "9.1.0" apply false
}

val nativeAbi = providers.gradleProperty("nativeAbi").orElse("arm64-v8a").get()
require(nativeAbi in setOf("arm64-v8a", "x86_64")) {
    "Unsupported nativeAbi '$nativeAbi'; use arm64-v8a or x86_64."
}

tasks.register<Delete>("cleanRustJniLibs") {
    delete(layout.projectDirectory.dir("app/src/main/jniLibs/$nativeAbi"))
}

tasks.register<Exec>("buildRustDebug") {
    group = "build"
    description = "Build the $nativeAbi Rust native library for Android."
    dependsOn("cleanRustJniLibs")
    workingDir = rootProject.projectDir.parentFile
    commandLine(
        "cargo", "ndk",
        "-t", nativeAbi,
        "-P", "26",
        "-o", "android/app/src/main/jniLibs",
        "build", "-p", "lyricmotion-android", "--lib"
    )
}

tasks.register<Delete>("cleanNativeDebugApk") {
    delete(layout.projectDirectory.file("app/build/outputs/apk/debug/app-debug.apk"))
}

tasks.register("assembleNativeDebug") {
    group = "build"
    description = "Build the Rust library and package the native Android debug APK."
    dependsOn("buildRustDebug")
    dependsOn("cleanNativeDebugApk")
    finalizedBy(":app:assembleDebug")
}

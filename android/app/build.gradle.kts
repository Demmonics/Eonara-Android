plugins {
    id("com.android.application")
}

val nativeAbi = providers.gradleProperty("nativeAbi").orElse("arm64-v8a").get()

android {
    namespace = "com.lyricmotion.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.lyricmotion.android"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += nativeAbi
        }
        manifestPlaceholders["nativeLibraryName"] = "lyricmotion_android"
    }

    buildTypes {
        debug {
            isDebuggable = true
            isJniDebuggable = true
        }
        release {
            isMinifyEnabled = false
        }
    }

}

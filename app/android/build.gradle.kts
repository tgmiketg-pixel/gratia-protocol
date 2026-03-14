plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.gratia.app"
    compileSdk = 34

    // WHY: NDK 27.1 is the version installed on this machine and provides
    // the ARM64 cross-compilation toolchain for the Rust core.
    ndkVersion = "27.1.12297006"

    defaultConfig {
        applicationId = "io.gratia.app"
        // WHY: minSdk 26 (Android 8.0) covers phones from 2017+ and gives us access
        // to all sensor APIs, foreground services, and JobScheduler features we need.
        // This aligns with the project target of $50+ phones manufactured after 2018.
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        // WHY: Only target ARM64 — Gratia is a mobile-only blockchain that requires
        // real ARM hardware for consensus. x86/x86_64 are emulator-only architectures
        // and are intentionally excluded.
        ndk {
            abiFilters += listOf("arm64-v8a")
        }

        vectorDrawables {
            useSupportLibrary = true
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isMinifyEnabled = false
            applicationIdSuffix = ".debug"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
    }

    composeOptions {
        // WHY: Kotlin compiler extension version must match the Compose BOM.
        // 1.5.8 is compatible with Kotlin 1.9.22 and Compose BOM 2024.01.00.
        kotlinCompilerExtensionVersion = "1.5.8"
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // Rust/UniFFI native library integration:
    //   1. Run `scripts/build-android.sh` to cross-compile the Rust core and
    //      generate Kotlin bindings.
    //   2. The .so is placed in src/main/jniLibs/arm64-v8a/libgratia_ffi.so
    //   3. UniFFI-generated Kotlin bindings are in src/main/kotlin/uniffi/gratia_ffi/
    //   4. JNA loads the native library at runtime via the generated bindings.
    //
    // The GratiaCoreManager bridge (io.gratia.app.bridge) wraps the UniFFI-generated
    // classes with a Kotlin-friendly API for the UI and service layers.
}

dependencies {
    // Jetpack Compose BOM — single version source for all Compose libraries
    val composeBom = platform("androidx.compose:compose-bom:2024.01.00")
    implementation(composeBom)
    androidTestImplementation(composeBom)

    // Compose UI
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    // Material Design 3
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")

    // Navigation
    implementation("androidx.navigation:navigation-compose:2.7.6")

    // Lifecycle + ViewModel for Compose
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.7.0")

    // Activity Compose integration
    implementation("androidx.activity:activity-compose:1.8.2")

    // Core KTX
    implementation("androidx.core:core-ktx:1.12.0")

    // Coroutines for async work (sensor data collection, Rust bridge calls)
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.7.3")

    // WHY: JNA (Java Native Access) is required by UniFFI-generated Kotlin bindings
    // to load and call into the Rust native library (libgratia_ffi.so).
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    // Testing
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}

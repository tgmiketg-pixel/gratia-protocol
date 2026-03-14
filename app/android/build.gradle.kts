plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.gratia.app"
    compileSdk = 34

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

    // TODO: Add Rust/UniFFI native library build integration.
    // This requires:
    //   1. Android NDK installed and configured (ndkVersion = "26.x.x")
    //   2. Rust cross-compilation target: aarch64-linux-android
    //   3. A Gradle task that runs `cargo build --target aarch64-linux-android --release`
    //      for the gratia-ffi crate and copies the resulting .so into jniLibs/
    //   4. UniFFI binding generation: `uniffi-bindgen generate` to produce the
    //      Kotlin bindings from the .udl file at crates/gratia-ffi/uniffi/gratia.udl
    //
    // For now, the GratiaCore bridge uses mock/placeholder data so the UI can
    // be developed independently of the Rust core.
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

    // Testing
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}

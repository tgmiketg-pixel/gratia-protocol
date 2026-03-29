plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.gratia.app"
    compileSdk = 35

    // WHY: NDK 27.1 is the version installed on this machine and provides
    // the ARM64 cross-compilation toolchain for the Rust core.
    ndkVersion = "27.1.12297006"

    defaultConfig {
        applicationId = "io.gratia.app"
        // WHY: minSdk 26 (Android 8.0) covers phones from 2017+ and gives us access
        // to all sensor APIs, foreground services, and JobScheduler features we need.
        // This aligns with the project target of $50+ phones manufactured after 2018.
        minSdk = 26
        targetSdk = 35
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
        buildConfig = true
    }

    composeOptions {
        // WHY: Kotlin compiler extension version must match the Compose BOM.
        // 1.5.14 is compatible with Kotlin 1.9.22 and Compose BOM 2024.06.00.
        kotlinCompilerExtensionVersion = "1.5.14"
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
    // WHY: BOM 2024.06.00 aligns all Compose libraries (Material3 1.2.x,
    // animation-core, foundation, etc.) to avoid NoSuchMethodError crashes
    // from version mismatches between Compose sub-libraries.
    val composeBom = platform("androidx.compose:compose-bom:2024.06.00")
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

    // WHY: Fragment 1.3.0+ required for registerForActivityResult in ComponentActivity.
    // Release lint (lintVital) enforces this — debug builds don't check.
    implementation("androidx.fragment:fragment-ktx:1.6.2")

    // Core KTX
    implementation("androidx.core:core-ktx:1.12.0")

    // Coroutines for async work (sensor data collection, Rust bridge calls)
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.7.3")

    // WHY: JNA (Java Native Access) is required by UniFFI-generated Kotlin bindings
    // to load and call into the Rust native library (libgratia_ffi.so).
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    // WHY: WorkManager guarantees periodic task execution even when the app
    // is killed by the OS or the device is in doze mode. Used as a backup
    // heartbeat to restart ProofOfLifeService if it gets killed.
    implementation("androidx.work:work-runtime-ktx:2.10.0")

    // WHY: ZXing core for QR code generation in the Receive dialog.
    // The wallet address is a 69-character string (grat:<64 hex>) which is
    // impractical to type manually. QR codes enable phone-to-phone transfers
    // by scanning. Only the core library is needed (~500KB), not the full
    // Android integration library.
    implementation("com.google.zxing:core:3.5.3")

    // WHY: CameraX + ML Kit barcode scanning for the Send dialog QR scanner.
    // Allows scanning a recipient's QR code instead of pasting the address.
    implementation("androidx.camera:camera-camera2:1.3.1")
    implementation("androidx.camera:camera-lifecycle:1.3.1")
    implementation("androidx.camera:camera-view:1.3.1")
    implementation("com.google.mlkit:barcode-scanning:17.2.0")

    // WHY: BiometricPrompt provides fingerprint, face, and device credential
    // authentication on Android 6.0+ (API 23+). Falls back to PIN/pattern/password
    // on devices without biometric hardware. Covers the $50 phone target.
    implementation("androidx.biometric:biometric:1.1.0")

    // WHY: EncryptedSharedPreferences stores security credentials (hashed PIN,
    // hashed pattern) using AES-256 via Android Keystore. Even with root access,
    // the stored hashes can't be read without the Keystore key.
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    // Testing
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}

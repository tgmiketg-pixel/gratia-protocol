pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    // WHY: Pin plugin versions here so build.gradle.kts can use `id("...")` without version.
    // AGP 8.7.3 supports compileSdk 35+ (needed by work-runtime-ktx 2.10.0).
    // Compatible with Gradle 8.11.1 and Kotlin 1.9.24.
    plugins {
        id("com.android.application") version "8.7.3"
        id("org.jetbrains.kotlin.android") version "1.9.24"
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "Gratia"

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    // WHY: Pin plugin versions here so build.gradle.kts can use `id("...")` without version.
    // AGP 8.2.2 is compatible with Gradle 8.11, Kotlin 1.9.22, and Compose BOM 2024.01.00.
    plugins {
        id("com.android.application") version "8.2.2"
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

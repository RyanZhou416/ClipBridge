// Top-level build file.
//
// The Android Gradle plugin is declared on the buildscript classpath instead of
// through the `plugins {}` DSL so that Gradle dependency locking can pin the
// buildscript classpath itself (buildscript-gradle.lockfile). The app module
// applies the plugin by id and inherits this classpath.
//
// Keep the version below in sync with gradle/libs.versions.toml (agp);
// scripts/ci/android.sh fails the build if the two drift apart.
import org.gradle.api.artifacts.dsl.LockMode

buildscript {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:8.13.2")
    }
    dependencyLocking {
        lockAllConfigurations()
        // CI verifies locks read-only; STRICT refuses both mismatches and
        // missing lock state instead of silently adding dependencies.
        lockMode.set(LockMode.STRICT)
    }
}

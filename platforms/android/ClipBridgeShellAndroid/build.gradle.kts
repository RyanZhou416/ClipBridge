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

        // Security constraints for AGP 8.13.2's buildscript classpath.
        //
        // The pinned AGP/Gradle combination resolves libraries with known
        // critical/high advisories (Guava, Netty, BouncyCastle, JDOM2, jose4j).
        // These constraints select the minimal fixed versions inside the same
        // major/line so the OSV gate can stay a blocking gate without ignoring
        // findings. Google does not support overriding AGP's internal graph, so
        // every constraint change must be validated by the full Android build
        // (help/tasks/buildEnvironment/signingReport, debug/release/AAB, JNI,
        // apksigner) and the buildscript lock must be regenerated.
        constraints {
            classpath("com.google.guava:guava:33.7.2-jre")
            classpath("io.netty:netty-buffer:4.1.137.Final")
            classpath("io.netty:netty-codec:4.1.137.Final")
            classpath("io.netty:netty-codec-http:4.1.137.Final")
            classpath("io.netty:netty-codec-http2:4.1.137.Final")
            classpath("io.netty:netty-codec-socks:4.1.137.Final")
            classpath("io.netty:netty-common:4.1.137.Final")
            classpath("io.netty:netty-handler:4.1.137.Final")
            classpath("io.netty:netty-handler-proxy:4.1.137.Final")
            classpath("io.netty:netty-resolver:4.1.137.Final")
            classpath("io.netty:netty-transport:4.1.137.Final")
            classpath("io.netty:netty-transport-native-unix-common:4.1.137.Final")
            classpath("org.bouncycastle:bcprov-jdk18on:1.85")
            classpath("org.bouncycastle:bcpkix-jdk18on:1.85")
            classpath("org.bouncycastle:bcutil-jdk18on:1.85")
            classpath("org.jdom:jdom2:2.0.6.1")
            classpath("org.bitbucket.b_c:jose4j:0.9.6")
            // Keep AGP's baseline annotation version instead of letting the
            // BouncyCastle 1.85 POM downgrade it via conflict resolution.
            classpath("org.checkerframework:checker-qual:3.43.0")
        }
    }
    dependencyLocking {
        lockAllConfigurations()
        // CI verifies locks read-only; STRICT refuses both mismatches and
        // missing lock state instead of silently adding dependencies.
        lockMode.set(LockMode.STRICT)
    }
}

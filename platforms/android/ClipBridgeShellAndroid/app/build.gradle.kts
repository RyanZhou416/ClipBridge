import org.gradle.api.artifacts.dsl.LockMode

plugins {
    id("com.android.application")
}

// ---------------------------------------------------------------------------
// Rust cdylib -> Android ABI contract
// ---------------------------------------------------------------------------
// The Android build never guesses which architectures to package. CI passes
// -Pclipbridge.android.abis=arm64-v8a,x86_64, and every ABI in that list must
// have a freshly built .so; a missing file fails configuration instead of
// silently shrinking the verification set.
//
// Local builds without the property keep the historical existence-based
// fallback so developers can build only the ABI they cross-compiled.
val repoRoot = layout.projectDirectory.dir("../../../../")
val rustSoByAbi = mapOf(
    "arm64-v8a" to repoRoot.file("target/aarch64-linux-android/release/libcore_ffi_android.so"),
    "x86_64" to repoRoot.file("target/x86_64-linux-android/release/libcore_ffi_android.so"),
)

val requestedAbis: Set<String>? = (
    providers.gradleProperty("clipbridge.android.abis").orNull
        ?: providers.environmentVariable("CLIPBRIDGE_ANDROID_ABIS").orNull
    )
    ?.split(",")
    ?.map { it.trim() }
    ?.filter { it.isNotEmpty() }
    ?.toSet()
    ?.takeIf { it.isNotEmpty() }

val availableAbis: Set<String> = if (requestedAbis != null) {
    requestedAbis.forEach { abi ->
        val so = rustSoByAbi[abi]
            ?: throw GradleException(
                "Unknown ABI '$abi' in -Pclipbridge.android.abis; supported: ${rustSoByAbi.keys.sorted()}"
            )
        if (!so.asFile.isFile) {
            throw GradleException(
                "Requested ABI '$abi' has no Rust library at ${so.asFile.absolutePath}. " +
                    "Build it first (cargo build -p core-ffi-android --release --locked --target <triple>)."
            )
        }
    }
    requestedAbis
} else {
    val detected = rustSoByAbi.filterValues { it.asFile.isFile }.keys
    if (detected.isEmpty()) {
        println("⚠ Warning: No Rust .so files found, defaulting to arm64-v8a only")
        setOf("arm64-v8a")
    } else {
        println("✓ Will build for ABIs: ${detected.sorted().joinToString(", ")}")
        detected
    }
}

android {
    ndkVersion = "26.3.11579264"
    namespace = "com.ryan416.clipbridgeshellandroid"
    compileSdk {
        version = release(36)
    }
    buildFeatures {
        aidl = true
    }
    defaultConfig {
        applicationId = "com.ryan416.clipbridgeshellandroid"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        ndk {
            // Exactly the validated ABI set; never "whatever library exists".
            abiFilters += availableAbis
        }

        externalNativeBuild {
            cmake {
                // arguments += listOf("-DANDROID_STL=c++_shared")
                // cppFlags += listOf("-std=c++17")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
        }
    }

    packaging {
        jniLibs {
            // Keep the prebuilt Rust .so byte-identical inside the APK so CI can
            // compare the packaged entry with the library built from this checkout.
            keepDebugSymbols.add("**/libcore_ffi_android.so")
        }
    }
}

dependencies {
    implementation(libs.appcompat)
    implementation(libs.material)
    implementation(libs.activity)
    implementation(libs.constraintlayout)
    implementation(libs.jna)
    testImplementation(libs.junit)
    androidTestImplementation(libs.ext.junit)
    androidTestImplementation(libs.espresso.core)
    implementation(libs.api)
    implementation(libs.provider)
    implementation(libs.hiddenapibypass)
}

val copyRustSo by tasks.registering(Copy::class) {
    duplicatesStrategy = DuplicatesStrategy.INCLUDE
    into(layout.projectDirectory.dir("src/main/jniLibs"))

    availableAbis.sorted().forEach { abi ->
        from(rustSoByAbi.getValue(abi)) {
            into(abi)
        }
    }
    outputs.dir(layout.projectDirectory.dir("src/main/jniLibs"))

    doFirst {
        println("=== Copying Rust .so files for ${availableAbis.sorted().joinToString(", ")} ===")
        availableAbis.sorted().forEach { abi ->
            println("  $abi: ${rustSoByAbi.getValue(abi).asFile.absolutePath} -> src/main/jniLibs/$abi")
        }
    }
}
tasks.named("preBuild") {
    dependsOn(copyRustSo)
}

tasks.matching {
    it.name.contains("merge") && it.name.contains("JniLibFolders")
}.configureEach {
    dependsOn(copyRustSo)
}

// ---------------------------------------------------------------------------
// Dependency locking (app debug/release compile + runtime classpaths)
// ---------------------------------------------------------------------------
// Android test classpaths are deliberately not resolved here: resolving every
// resolvable configuration makes AGP try to choose between :app's own
// data-binding/feature variants and fails. The app's debug/release compile and
// runtime graphs are what the APK and the OSV scan consume.
val lockedConfigurationNames = setOf(
    "debugCompileClasspath",
    "debugRuntimeClasspath",
    "releaseCompileClasspath",
    "releaseRuntimeClasspath",
)

configurations.configureEach {
    if (isCanBeResolved && name in lockedConfigurationNames) {
        resolutionStrategy.activateDependencyLocking()
    }
}

dependencyLocking {
    // CI verifies the committed lock state read-only; STRICT also fails when a
    // locked configuration has no lock state instead of resolving unrecorded.
    lockMode.set(LockMode.STRICT)
}

// Generate/refresh (requires the full Android toolchain, not CI verification):
//   (cd platforms/android/ClipBridgeShellAndroid && ./gradlew --write-locks help)
//   (cd platforms/android/ClipBridgeShellAndroid && ./gradlew :app:resolveAndLockAll --write-locks)
tasks.register("resolveAndLockAll") {
    notCompatibleWithConfigurationCache("Resolves the locked configurations to write lock state")
    doFirst {
        require(gradle.startParameter.isWriteDependencyLocks) {
            "Run with --write-locks: ./gradlew :app:resolveAndLockAll --write-locks"
        }
    }
    doLast {
        configurations
            .filter { it.isCanBeResolved && it.name in lockedConfigurationNames }
            .forEach { it.resolve() }
    }
}

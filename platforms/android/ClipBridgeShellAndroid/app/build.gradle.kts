import org.gradle.api.tasks.Copy
plugins {
    alias(libs.plugins.android.application)
}

// 检查哪些架构的库文件存在（需要在 android 块之前定义）
// 使用 Gradle 的跨平台路径处理
val repoRoot = layout.projectDirectory.dir("../../../../")
val arm64So = repoRoot.file("target/aarch64-linux-android/release/libcore_ffi_android.so")
val x86_64So = repoRoot.file("target/x86_64-linux-android/release/libcore_ffi_android.so")

// 在配置阶段检查文件存在性（跨平台兼容）
val availableAbis = mutableSetOf<String>()
val arm64Exists = arm64So.asFile.exists()
val x86_64Exists = x86_64So.asFile.exists()

if (arm64Exists) {
	availableAbis.add("arm64-v8a")
	println("✓ Found arm64-v8a library: ${arm64So.asFile.absolutePath}")
} else {
	println("✗ arm64-v8a library not found: ${arm64So.asFile.absolutePath}")
}

if (x86_64Exists) {
	availableAbis.add("x86_64")
	println("✓ Found x86_64 library: ${x86_64So.asFile.absolutePath}")
} else {
	println("✗ x86_64 library not found: ${x86_64So.asFile.absolutePath}")
}

// 如果没有任何库，默认至少尝试 arm64-v8a（用于开发时）
if (availableAbis.isEmpty()) {
	availableAbis.add("arm64-v8a")
	println("⚠ Warning: No Rust .so files found, defaulting to arm64-v8a only")
} else {
	println("✓ Will build for ABIs: ${availableAbis.joinToString(", ")}")
}

android {
	ndkVersion = "26.3.11579264"
    namespace = "com.ryan416.clipbridgeshellandroid"
    compileSdk {
        version = release(36)
    }
	buildFeatures {
		aidl = true  // 确保这一行是 true
	}
    defaultConfig {
        applicationId = "com.ryan416.clipbridgeshellandroid"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

		ndk {
			// 只在有对应架构的库时才构建该架构
			abiFilters += availableAbis
		}

		externalNativeBuild {
			cmake {
				// 可选：传一些 CMake 参数
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
			// version = "3.22.1" // 可选：如果你想锁定版本
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

	// 1️⃣ 明确这是一个"复制任务"，允许同名 so
	duplicatesStrategy = DuplicatesStrategy.INCLUDE

	// 2️⃣ 使用已定义的源文件路径（已在上面定义）

	// 3️⃣ 只声明存在的文件为输入
	if (arm64So.asFile.exists()) {
		inputs.file(arm64So)
	}
	if (x86_64So.asFile.exists()) {
		inputs.file(x86_64So)
	}

	// 4️⃣ 明确声明 outputs
	outputs.dir(layout.projectDirectory.dir("src/main/jniLibs/arm64-v8a"))
	outputs.dir(layout.projectDirectory.dir("src/main/jniLibs/x86_64"))

	// 5️⃣ 只在文件存在时才复制
	if (arm64So.asFile.exists()) {
		from(arm64So) {
			into("arm64-v8a")
		}
	}

	if (x86_64So.asFile.exists()) {
		from(x86_64So) {
			into("x86_64")
		}
	}

	into(layout.projectDirectory.dir("src/main/jniLibs"))

	doFirst {
		// 在执行时再次检查（确保文件在构建时存在）
		println("=== Copying Rust .so files ===")
		val arm64ExistsNow = arm64So.asFile.exists()
		val x86_64ExistsNow = x86_64So.asFile.exists()
		
		if (arm64ExistsNow) {
			println("  ✓ arm64-v8a: ${arm64So.asFile.absolutePath}")
			println("     -> src/main/jniLibs/arm64-v8a")
		} else {
			println("  ✗ arm64-v8a: SKIPPED (file not found at ${arm64So.asFile.absolutePath})")
		}
		
		if (x86_64ExistsNow) {
			println("  ✓ x86_64: ${x86_64So.asFile.absolutePath}")
			println("     -> src/main/jniLibs/x86_64")
		} else {
			println("  ✗ x86_64: SKIPPED (file not found at ${x86_64So.asFile.absolutePath})")
		}
		println("==============================")
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

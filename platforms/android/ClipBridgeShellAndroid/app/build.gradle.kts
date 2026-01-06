import org.gradle.api.tasks.Copy
plugins {
    alias(libs.plugins.android.application)
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
	defaultConfig {
		// 你已有的 applicationId/minSdk/targetSdk/versionCode/versionName 保留

		ndk {
			abiFilters += setOf("arm64-v8a", "x86_64")
		}

		externalNativeBuild {
			cmake {
				// 可选：传一些 CMake 参数
				// arguments += listOf("-DANDROID_STL=c++_shared")
				// cppFlags += listOf("-std=c++17")
			}
		}
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
val repoRoot = layout.projectDirectory.dir("../../../../")

val copyRustSo by tasks.registering(Copy::class) {

	// 1️⃣ 明确这是一个“复制任务”，允许同名 so
	duplicatesStrategy = DuplicatesStrategy.INCLUDE

	// 2️⃣ 明确声明 inputs（非常重要）
	inputs.file(repoRoot.file("target/aarch64-linux-android/release/libcore_ffi_android.so"))
	inputs.file(repoRoot.file("target/x86_64-linux-android/release/libcore_ffi_android.so"))

	// 3️⃣ 明确声明 outputs（这是你现在缺的）
	outputs.dir(layout.projectDirectory.dir("src/main/jniLibs/arm64-v8a"))
	outputs.dir(layout.projectDirectory.dir("src/main/jniLibs/x86_64"))

	from(repoRoot.file("target/aarch64-linux-android/release/libcore_ffi_android.so")) {
		into("arm64-v8a")
	}

	from(repoRoot.file("target/x86_64-linux-android/release/libcore_ffi_android.so")) {
		into("x86_64")
	}

	into(layout.projectDirectory.dir("src/main/jniLibs"))

	doFirst {
		println("Copying Rust .so:")
		println("  arm64  -> src/main/jniLibs/arm64-v8a")
		println("  x86_64 -> src/main/jniLibs/x86_64")
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

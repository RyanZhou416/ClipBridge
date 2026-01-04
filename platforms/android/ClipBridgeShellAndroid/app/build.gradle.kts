import org.gradle.api.tasks.Copy
plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "com.ryan416.clipbridgeshellandroid"
    compileSdk {
        version = release(36)
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
}

dependencies {
    implementation(libs.appcompat)
    implementation(libs.material)
    implementation(libs.activity)
    implementation(libs.constraintlayout)
    testImplementation(libs.junit)
    androidTestImplementation(libs.ext.junit)
    androidTestImplementation(libs.espresso.core)
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

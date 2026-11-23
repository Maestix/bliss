import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

val repo = rootDir.parentFile
val desktopAssets = File(repo, "crates/app/assets")

// The phone's colours are the desktop's (docs/android-stack.md): every
// `--color-*: #rrggbb;` in theme.css becomes a constant, so the two cannot
// drift. Radii and spacing are the phone's own and live in Theme.kt.
val tokens = tasks.register("blissTokens") {
    val css = File(desktopAssets, "theme.css")
    val out = layout.buildDirectory.dir("generated/tokens/app/bliss")
    inputs.file(css)
    outputs.dir(out)
    doLast {
        val pattern = Regex("""--color-([a-z0-9-]+):\s*#([0-9a-fA-F]{6})\s*;""")
        val lines = pattern.findAll(css.readText()).map { m ->
            val name = m.groupValues[1].uppercase().replace('-', '_')
            "    val $name = Color(0xFF${m.groupValues[2].uppercase()})"
        }.distinct().joinToString("\n")
        val file = out.get().file("Tokens.kt").asFile
        file.parentFile.mkdirs()
        file.writeText(
            "// Generated from crates/app/assets/theme.css by the blissTokens task.\n" +
                "package app.bliss\n\nimport androidx.compose.ui.graphics.Color\n\n" +
                "object Tokens {\n$lines\n}\n"
        )
    }
}

// The desktop's own pictures, under names Android accepts.
val pictures = tasks.register<Copy>("blissPictures") {
    from(desktopAssets) {
        include("status-card.jpg", "rail-cat.jpg", "tray-bunny.jpg", "bliss-mark.png")
        rename("status-card.jpg", "scene.jpg")
        rename("rail-cat.jpg", "cat.jpg")
        rename("tray-bunny.jpg", "bunny.jpg")
        rename("bliss-mark.png", "mark.png")
    }
    into(layout.buildDirectory.dir("generated/pictures/drawable-nodpi"))
}

android {
    namespace = "app.bliss"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.bliss"
        minSdk = 29
        targetSdk = 36
        versionCode = 30
        versionName = "0.5.14"
        ndk { abiFilters += "arm64-v8a" }
    }

    // One key for every build, kept in the repository (a private one, at the
    // user's word): the debug key of the machine that built every APK before
    // GitHub Actions did, so a build from anywhere installs over any other
    // and keeps the app's data. Debug builds take it too.
    signingConfigs {
        getByName("debug") {
            storeFile = File(rootDir, "bliss.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { compose = true; buildConfig = true }

    sourceSets["main"].java.srcDir(layout.buildDirectory.dir("generated/tokens/app"))
    sourceSets["main"].res.srcDir(layout.buildDirectory.dir("generated/pictures"))

    // xray is an executable shipped as libxray.so: Android runs files only
    // from the folder it unpacks native libraries into, and unpacks them only
    // when they are stored the old way.
    packaging { jniLibs { useLegacyPackaging = true } }
}

kotlin { compilerOptions { jvmTarget.set(JvmTarget.JVM_17) } }

tasks.named("preBuild") { dependsOn(tokens, pictures) }

dependencies {
    val bom = platform("androidx.compose:compose-bom:2025.10.00")
    implementation(bom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.11.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.4")
    implementation("androidx.core:core-ktx:1.17.0")
    // QR import: the camera through CameraX, the code read by ZXing's core
    // (Apache 2.0, no Google Play services, works offline).
    val camera = "1.5.1"
    implementation("androidx.camera:camera-camera2:$camera")
    implementation("androidx.camera:camera-lifecycle:$camera")
    implementation("androidx.camera:camera-view:$camera")
    implementation("com.google.zxing:core:3.5.3")
}
